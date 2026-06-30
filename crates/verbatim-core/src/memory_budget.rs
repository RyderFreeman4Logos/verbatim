use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::config::{DaemonResourceConfig, MemoryBudgetEnforcement};

const BYTES_PER_MIB: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct MemoryBudget {
    inner: Arc<RwLock<MemoryBudgetInner>>,
}

struct MemoryBudgetInner {
    limit_mb: Option<usize>,
    enforcement: MemoryBudgetEnforcement,
    poll_interval: Duration,
    margin_percent: u8,
    reservations: HashMap<String, MemoryReservation>,
    current_rss_bytes: u64,
}

struct MemoryReservation {
    owner: String,
    estimated_mb: usize,
    reserved_at: Instant,
}

pub struct MemoryReservationGuard {
    budget: MemoryBudget,
    key: String,
    owner: String,
    estimated_mb: usize,
    degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryBudgetSnapshot {
    pub limit_mb: Option<usize>,
    pub rss_mb: u64,
    pub reserved_mb: usize,
    pub available_mb: Option<usize>,
    pub enforcement: MemoryBudgetEnforcement,
    pub active_reservations: Vec<MemoryReservationSnapshot>,
}

impl Default for MemoryBudgetSnapshot {
    fn default() -> Self {
        Self {
            limit_mb: None,
            rss_mb: 0,
            reserved_mb: 0,
            available_mb: None,
            enforcement: MemoryBudgetEnforcement::SlowWarn,
            active_reservations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReservationSnapshot {
    pub key: String,
    pub owner: String,
    pub estimated_mb: usize,
    pub reserved_for_millis: u64,
}

impl MemoryBudget {
    pub fn new(
        limit_mb: Option<usize>,
        enforcement: MemoryBudgetEnforcement,
        poll_interval: Duration,
        margin_percent: u8,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(MemoryBudgetInner {
                limit_mb,
                enforcement,
                poll_interval: poll_interval.max(Duration::from_millis(1)),
                margin_percent: margin_percent.min(100),
                reservations: HashMap::new(),
                current_rss_bytes: sample_current_rss_bytes().unwrap_or(0),
            })),
        }
    }

    pub fn from_config(config: &DaemonResourceConfig) -> Self {
        let config = config.bounded();
        Self::new(
            config.memory_budget_mb,
            config.memory_budget_enforcement,
            Duration::from_millis(config.memory_budget_poll_millis),
            config.memory_reservation_margin_percent,
        )
    }

    pub fn configure_from(&self, config: &DaemonResourceConfig) -> Result<()> {
        let config = config.bounded();
        let mut inner = self
            .inner
            .write()
            .map_err(|error| anyhow::anyhow!("memory budget lock poisoned: {error}"))?;
        inner.limit_mb = config.memory_budget_mb;
        inner.enforcement = config.memory_budget_enforcement;
        inner.poll_interval = Duration::from_millis(config.memory_budget_poll_millis);
        inner.margin_percent = config.memory_reservation_margin_percent;
        Ok(())
    }

    pub fn update_limit(&self, limit_mb: Option<usize>) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|error| anyhow::anyhow!("memory budget lock poisoned: {error}"))?;
        inner.limit_mb = limit_mb;
        Ok(())
    }

    pub fn update_enforcement(&self, enforcement: MemoryBudgetEnforcement) -> Result<()> {
        let mut inner = self
            .inner
            .write()
            .map_err(|error| anyhow::anyhow!("memory budget lock poisoned: {error}"))?;
        inner.enforcement = enforcement;
        Ok(())
    }

    pub fn try_reserve(
        &self,
        key: impl Into<String>,
        owner: impl Into<String>,
        estimated_mb: usize,
    ) -> Result<MemoryReservationGuard> {
        let key = key.into();
        let owner = owner.into();
        let mut inner = self
            .inner
            .write()
            .map_err(|error| anyhow::anyhow!("memory budget lock poisoned: {error}"))?;
        if inner.reservations.contains_key(&key) {
            bail!("memory reservation already exists: {key}");
        }

        let used_bytes = inner.used_bytes();
        let projected_bytes = used_bytes.saturating_add(mb_to_bytes(estimated_mb));
        let over_admission_limit = inner
            .admission_limit_bytes()
            .is_some_and(|limit| projected_bytes > limit);
        let degraded =
            matches!(inner.enforcement, MemoryBudgetEnforcement::SlowWarn) && over_admission_limit;

        match inner.enforcement {
            MemoryBudgetEnforcement::Off => {
                if over_admission_limit {
                    warn_memory_pressure(&inner, &owner, estimated_mb, projected_bytes);
                }
            }
            MemoryBudgetEnforcement::Warn => {
                if over_admission_limit {
                    warn_memory_pressure(&inner, &owner, estimated_mb, projected_bytes);
                }
            }
            MemoryBudgetEnforcement::SlowWarn => {
                if over_admission_limit {
                    warn_memory_pressure(&inner, &owner, estimated_mb, projected_bytes);
                }
            }
            MemoryBudgetEnforcement::Defer => {
                if over_admission_limit {
                    bail!(
                        "memory budget unavailable; deferring {owner}: requested {estimated_mb} MB, projected {} MB, available {:?} MB",
                        bytes_to_mb(projected_bytes),
                        inner.available_mb()
                    );
                }
            }
            MemoryBudgetEnforcement::Fail => {
                if over_admission_limit {
                    bail!(
                        "memory budget exceeded for {owner}: requested {estimated_mb} MB, projected {} MB, available {:?} MB",
                        bytes_to_mb(projected_bytes),
                        inner.available_mb()
                    );
                }
            }
        }

        inner.reservations.insert(
            key.clone(),
            MemoryReservation {
                owner: owner.clone(),
                estimated_mb,
                reserved_at: Instant::now(),
            },
        );
        Ok(MemoryReservationGuard {
            budget: self.clone(),
            key,
            owner,
            estimated_mb,
            degraded,
        })
    }

    pub fn release(&self, key: &str) {
        match self.inner.write() {
            Ok(mut inner) => {
                inner.reservations.remove(key);
            }
            Err(error) => {
                tracing::warn!(error = %error, key, "failed to release memory reservation");
            }
        }
    }

    pub fn available_mb(&self) -> Option<usize> {
        self.inner
            .read()
            .ok()
            .and_then(|inner| inner.available_mb())
    }

    pub fn reserved_mb(&self) -> usize {
        self.inner
            .read()
            .map(|inner| inner.reserved_mb())
            .unwrap_or_default()
    }

    pub fn rss_bytes(&self) -> u64 {
        self.inner
            .read()
            .map(|inner| inner.current_rss_bytes)
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> MemoryBudgetSnapshot {
        let Ok(inner) = self.inner.read() else {
            return MemoryBudgetSnapshot::default();
        };
        let now = Instant::now();
        let mut active_reservations = inner
            .reservations
            .iter()
            .map(|(key, reservation)| MemoryReservationSnapshot {
                key: key.clone(),
                owner: reservation.owner.clone(),
                estimated_mb: reservation.estimated_mb,
                reserved_for_millis: now
                    .duration_since(reservation.reserved_at)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            })
            .collect::<Vec<_>>();
        active_reservations.sort_by(|left, right| left.key.cmp(&right.key));
        MemoryBudgetSnapshot {
            limit_mb: inner.limit_mb,
            rss_mb: bytes_to_mb(inner.current_rss_bytes),
            reserved_mb: inner.reserved_mb(),
            available_mb: inner.available_mb(),
            enforcement: inner.enforcement,
            active_reservations,
        }
    }

    pub fn refresh_rss(&self) {
        let Some(bytes) = sample_current_rss_bytes() else {
            return;
        };
        match self.inner.write() {
            Ok(mut inner) => {
                inner.current_rss_bytes = bytes;
            }
            Err(error) => {
                tracing::warn!(error = %error, "failed to update memory budget RSS sample");
            }
        }
    }

    pub fn poll_interval(&self) -> Duration {
        self.inner
            .read()
            .map(|inner| inner.poll_interval)
            .unwrap_or_else(|_| Duration::from_millis(500))
    }

    pub fn start_rss_sampler(&self) -> tokio::task::JoinHandle<()> {
        let budget = self.clone();
        tokio::spawn(async move {
            loop {
                budget.refresh_rss();
                tokio::time::sleep(budget.poll_interval()).await;
            }
        })
    }

    #[cfg(test)]
    fn set_rss_bytes_for_test(&self, bytes: u64) {
        let mut inner = self.inner.write().expect("memory budget lock");
        inner.current_rss_bytes = bytes;
    }
}

impl MemoryReservationGuard {
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn estimated_mb(&self) -> usize {
        self.estimated_mb
    }
}

impl Drop for MemoryReservationGuard {
    fn drop(&mut self) {
        self.budget.release(&self.key);
    }
}

impl MemoryBudgetInner {
    fn reserved_mb(&self) -> usize {
        self.reservations
            .values()
            .fold(0_usize, |total, reservation| {
                total.saturating_add(reservation.estimated_mb)
            })
    }

    fn used_bytes(&self) -> u64 {
        self.current_rss_bytes
            .saturating_add(mb_to_bytes(self.reserved_mb()))
    }

    fn limit_bytes(&self) -> Option<u64> {
        self.limit_mb.map(mb_to_bytes)
    }

    fn admission_limit_bytes(&self) -> Option<u64> {
        let limit = u128::from(self.limit_bytes()?);
        let admitted_percent = u128::from(100_u8.saturating_sub(self.margin_percent.min(100)));
        Some(
            limit
                .saturating_mul(admitted_percent)
                .saturating_div(100)
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }

    fn available_mb(&self) -> Option<usize> {
        let available_bytes = self.limit_bytes()?.saturating_sub(self.used_bytes());
        usize::try_from(bytes_to_mb(available_bytes)).ok()
    }
}

fn warn_memory_pressure(
    inner: &MemoryBudgetInner,
    owner: &str,
    estimated_mb: usize,
    projected_bytes: u64,
) {
    tracing::warn!(
        owner,
        estimated_mb,
        projected_mb = bytes_to_mb(projected_bytes),
        limit_mb = ?inner.limit_mb,
        available_mb = ?inner.available_mb(),
        enforcement = ?inner.enforcement,
        "memory budget pressure detected"
    );
}

fn mb_to_bytes(mb: usize) -> u64 {
    let Ok(mb) = u64::try_from(mb) else {
        return u64::MAX;
    };
    mb.saturating_mul(BYTES_PER_MIB)
}

fn bytes_to_mb(bytes: u64) -> u64 {
    bytes / BYTES_PER_MIB
}

pub fn sample_current_rss_bytes() -> Option<u64> {
    sample_platform_rss_bytes()
}

#[cfg(target_os = "linux")]
fn sample_platform_rss_bytes() -> Option<u64> {
    read_proc_smaps_rollup_bytes().or_else(read_proc_status_rss_bytes)
}

#[cfg(target_os = "linux")]
fn read_proc_smaps_rollup_bytes() -> Option<u64> {
    let contents = fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    parse_proc_kb_value(&contents, "Pss:").or_else(|| parse_proc_kb_value(&contents, "Rss:"))
}

#[cfg(target_os = "linux")]
fn read_proc_status_rss_bytes() -> Option<u64> {
    let contents = fs::read_to_string("/proc/self/status").ok()?;
    parse_proc_kb_value(&contents, "VmRSS:")
}

#[cfg(target_os = "linux")]
fn parse_proc_kb_value(contents: &str, key: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.split_whitespace().next()?;
        let kb = value.parse::<u64>().ok()?;
        Some(kb.saturating_mul(1024))
    })
}

#[cfg(not(target_os = "linux"))]
fn sample_platform_rss_bytes() -> Option<u64> {
    use std::sync::Once;

    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        tracing::warn!(
            "RSS sampling is not supported on this platform; memory budget uses reservations only"
        );
    });
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(
        limit_mb: Option<usize>,
        enforcement: MemoryBudgetEnforcement,
        margin_percent: u8,
    ) -> MemoryBudget {
        let budget = MemoryBudget::new(
            limit_mb,
            enforcement,
            Duration::from_millis(500),
            margin_percent,
        );
        budget.set_rss_bytes_for_test(0);
        budget
    }

    #[test]
    fn reserve_release_lifecycle_updates_snapshot() {
        let budget = budget(Some(100), MemoryBudgetEnforcement::Fail, 25);
        let guard = budget
            .try_reserve("task-1:phase", "ingest:hnsw_build", 10)
            .unwrap();

        let snapshot = budget.snapshot();
        assert_eq!(snapshot.reserved_mb, 10);
        assert_eq!(snapshot.active_reservations.len(), 1);
        assert!(!guard.degraded());

        drop(guard);

        let snapshot = budget.snapshot();
        assert_eq!(snapshot.reserved_mb, 0);
        assert!(snapshot.active_reservations.is_empty());
    }

    #[test]
    fn slow_warn_marks_pressure_as_degraded_without_failing() {
        let budget = budget(Some(100), MemoryBudgetEnforcement::SlowWarn, 25);

        let guard = budget
            .try_reserve("task-1:phase", "ingest:full_index", 76)
            .unwrap();

        assert!(guard.degraded());
        assert_eq!(budget.reserved_mb(), 76);
    }

    #[test]
    fn defer_and_fail_reject_when_margin_admission_limit_is_exceeded() {
        for enforcement in [
            MemoryBudgetEnforcement::Defer,
            MemoryBudgetEnforcement::Fail,
        ] {
            let budget = budget(Some(100), enforcement, 25);
            let error = match budget.try_reserve("task-1:phase", "ingest:full_index", 76) {
                Ok(_) => panic!("expected memory budget rejection"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("memory budget"),
                "unexpected error: {error:#}"
            );
            assert_eq!(budget.reserved_mb(), 0);
        }
    }

    #[test]
    fn warn_and_off_track_without_rejecting() {
        for enforcement in [MemoryBudgetEnforcement::Warn, MemoryBudgetEnforcement::Off] {
            let budget = budget(Some(100), enforcement, 25);
            let guard = budget
                .try_reserve("task-1:phase", "ingest:full_index", 200)
                .unwrap();

            assert!(!guard.degraded());
            assert_eq!(budget.reserved_mb(), 200);
        }
    }

    #[test]
    fn hot_reload_limit_changes_available_budget() {
        let budget = budget(Some(100), MemoryBudgetEnforcement::Fail, 0);
        let _guard = budget.try_reserve("task-1:phase", "ingest", 60).unwrap();

        assert_eq!(budget.available_mb(), Some(40));

        budget.update_limit(Some(80)).unwrap();

        assert_eq!(budget.available_mb(), Some(20));
    }

    #[test]
    fn margin_controls_admission_threshold() {
        let budget = budget(Some(100), MemoryBudgetEnforcement::SlowWarn, 25);

        let within = budget.try_reserve("within", "ingest", 75).unwrap();
        assert!(!within.degraded());
        drop(within);

        let over = budget.try_reserve("over", "ingest", 76).unwrap();
        assert!(over.degraded());
    }

    #[test]
    fn duplicate_reservation_keys_are_rejected() {
        let budget = budget(Some(100), MemoryBudgetEnforcement::Off, 0);
        let _guard = budget.try_reserve("same", "ingest", 1).unwrap();

        let error = match budget.try_reserve("same", "ingest", 1) {
            Ok(_) => panic!("expected duplicate reservation rejection"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("already exists"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_proc_kb_values() {
        let contents = "Name:\ttest\nPss:\t123 kB\nRss:\t456 kB\n";

        assert_eq!(parse_proc_kb_value(contents, "Pss:"), Some(123 * 1024));
        assert_eq!(parse_proc_kb_value(contents, "VmRSS:"), None);
    }
}
