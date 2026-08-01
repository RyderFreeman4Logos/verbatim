use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::config::{DaemonResourceConfig, MemoryBudgetEnforcement};

mod health_snapshot;

pub use health_snapshot::{MemoryBudgetSnapshot, MemoryReservationSnapshot, MemoryUsageSource};

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
    current_usage: MemoryUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemoryUsage {
    bytes: u64,
    source: MemoryUsageSource,
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
                current_usage: sample_current_memory_usage().unwrap_or(MemoryUsage {
                    bytes: 0,
                    source: MemoryUsageSource::Unavailable,
                }),
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
                        "memory budget unavailable; deferring {owner}: requested {estimated_mb} MB, projected {} MB, available {:?} MB, source {}",
                        bytes_to_mb(projected_bytes),
                        inner.available_mb(),
                        inner.current_usage.source.as_str()
                    );
                }
            }
            MemoryBudgetEnforcement::Fail => {
                if over_admission_limit {
                    bail!(
                        "memory budget exceeded for {owner}: requested {estimated_mb} MB, projected {} MB, available {:?} MB, source {}",
                        bytes_to_mb(projected_bytes),
                        inner.available_mb(),
                        inner.current_usage.source.as_str()
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

    pub fn used_memory_bytes(&self) -> u64 {
        self.inner
            .read()
            .map(|inner| inner.current_usage.bytes)
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
            used_memory_mb: bytes_to_mb(inner.current_usage.bytes),
            usage_source: inner.current_usage.source,
            reserved_mb: inner.reserved_mb(),
            available_mb: inner.available_mb(),
            enforcement: inner.enforcement,
            active_reservations,
        }
    }

    pub fn refresh_memory_usage(&self) {
        let usage = sample_current_memory_usage();
        self.update_memory_usage(usage);
    }

    fn update_memory_usage(&self, usage: Option<MemoryUsage>) {
        let Some(usage) = usage else {
            return;
        };
        match self.inner.write() {
            Ok(mut inner) => {
                inner.current_usage = usage;
            }
            Err(error) => {
                tracing::warn!(error = %error, "failed to update memory budget usage sample");
            }
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

    pub fn start_memory_sampler(&self) -> tokio::task::JoinHandle<()> {
        let budget = self.clone();
        tokio::spawn(async move {
            loop {
                budget.refresh_memory_usage();
                budget.refresh_rss();
                tokio::time::sleep(budget.poll_interval()).await;
            }
        })
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

    #[cfg(test)]
    fn set_memory_usage_for_test(&self, usage: MemoryUsage) {
        let mut inner = self.inner.write().expect("memory budget lock");
        inner.current_usage = usage;
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
        self.current_usage
            .bytes
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
        usage_source = inner.current_usage.source.as_str(),
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

fn sample_current_memory_usage() -> Option<MemoryUsage> {
    sample_platform_memory_usage()
}

pub fn sample_current_rss_bytes() -> Option<u64> {
    sample_platform_rss_bytes()
}

#[cfg(target_os = "linux")]
fn sample_platform_memory_usage() -> Option<MemoryUsage> {
    sample_linux_memory_usage_with(|path| fs::read_to_string(path).ok())
}

#[cfg(target_os = "linux")]
fn sample_platform_rss_bytes() -> Option<u64> {
    read_proc_rss_bytes_with(&|path| fs::read_to_string(path).ok())
}

#[cfg(target_os = "linux")]
fn sample_linux_memory_usage_with(
    read_to_string: impl Fn(&Path) -> Option<String>,
) -> Option<MemoryUsage> {
    let cgroup = read_to_string(Path::new("/proc/self/cgroup"));
    let mountinfo = read_to_string(Path::new("/proc/self/mountinfo"));
    if let (Some(cgroup), Some(mountinfo)) = (cgroup, mountinfo) {
        if let Some(path) = resolve_cgroup_v2_memory_current_path(&cgroup, &mountinfo) {
            if let Some(bytes) = read_to_string(&path).and_then(|value| parse_bytes(&value)) {
                return Some(MemoryUsage {
                    bytes,
                    source: MemoryUsageSource::CgroupV2,
                });
            }
        }
    }

    read_proc_rss_bytes_with(&read_to_string).map(|bytes| MemoryUsage {
        bytes,
        source: MemoryUsageSource::RssFallback,
    })
}

#[cfg(target_os = "linux")]
fn resolve_cgroup_v2_memory_current_path(cgroup: &str, mountinfo: &str) -> Option<PathBuf> {
    let cgroup_path = parse_unified_cgroup_path(cgroup)?;
    mountinfo
        .lines()
        .filter_map(|line| parse_cgroup2_mount(line, &cgroup_path))
        .max_by_key(|(root_depth, _)| *root_depth)
        .map(|(_, path)| path.join("memory.current"))
}

#[cfg(target_os = "linux")]
fn parse_unified_cgroup_path(contents: &str) -> Option<PathBuf> {
    let mut unified = contents.lines().filter_map(|line| {
        let mut fields = line.splitn(3, ':');
        match (fields.next(), fields.next(), fields.next()) {
            (Some("0"), Some(""), Some(path)) => decode_clean_absolute_path(path),
            _ => None,
        }
    });
    let path = unified.next()?;
    if unified.next().is_some() {
        return None;
    }
    Some(path)
}

#[cfg(target_os = "linux")]
fn parse_cgroup2_mount(line: &str, cgroup_path: &Path) -> Option<(usize, PathBuf)> {
    let (mount, filesystem) = line.split_once(" - ")?;
    if filesystem.split_whitespace().next()? != "cgroup2" {
        return None;
    }
    let fields = mount.split_whitespace().collect::<Vec<_>>();
    let root = decode_clean_absolute_path(fields.get(3)?)?;
    let mount_point = decode_clean_absolute_path(fields.get(4)?)?;
    let relative = cgroup_path.strip_prefix(&root).ok()?;
    Some((root.components().count(), mount_point.join(relative)))
}

#[cfg(target_os = "linux")]
fn decode_clean_absolute_path(encoded: &str) -> Option<PathBuf> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let digits = bytes.get(index + 1..index + 4)?;
        if !digits.iter().all(u8::is_ascii_digit) || digits.iter().any(|digit| *digit > b'7') {
            return None;
        }
        let value = u16::from(digits[0] - b'0') * 64
            + u16::from(digits[1] - b'0') * 8
            + u16::from(digits[2] - b'0');
        decoded.push(u8::try_from(value).ok()?);
        index += 4;
    }
    let path = PathBuf::from(String::from_utf8(decoded).ok()?);
    let clean = path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        });
    clean.then_some(path)
}

#[cfg(target_os = "linux")]
fn parse_bytes(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn read_proc_rss_bytes_with(read_to_string: &impl Fn(&Path) -> Option<String>) -> Option<u64> {
    read_to_string(Path::new("/proc/self/smaps_rollup"))
        .and_then(|contents| parse_proc_kb_value(&contents, "Rss:"))
        .or_else(|| {
            read_to_string(Path::new("/proc/self/status"))
                .and_then(|contents| parse_proc_kb_value(&contents, "VmRSS:"))
        })
}

#[cfg(target_os = "linux")]
fn parse_proc_kb_value(contents: &str, key: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.split_whitespace().next()?;
        let kb = value.parse::<u64>().ok()?;
        kb.checked_mul(1024)
    })
}

#[cfg(not(target_os = "linux"))]
fn sample_platform_memory_usage() -> Option<MemoryUsage> {
    use std::sync::Once;

    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        tracing::warn!(
            "memory sampling is not supported on this platform; memory budget uses reservations only"
        );
    });
    None
}

#[cfg(not(target_os = "linux"))]
fn sample_platform_rss_bytes() -> Option<u64> {
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
        budget.set_memory_usage_for_test(MemoryUsage {
            bytes: 0,
            source: MemoryUsageSource::RssFallback,
        });
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
        assert_eq!(
            read_proc_rss_bytes_with(&|path| {
                (path == Path::new("/proc/self/smaps_rollup")).then(|| contents.to_string())
            }),
            Some(456 * 1024)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_v2_current_preferred_for_admission() {
        use std::collections::HashMap;
        use std::path::PathBuf;

        let files = HashMap::from([
            (
                PathBuf::from("/proc/self/cgroup"),
                "0::/workload.slice/tenant/group\n".to_string(),
            ),
            (
                PathBuf::from("/proc/self/mountinfo"),
                "29 23 0:26 /workload.slice /run/cgroup\\040v2 rw - cgroup2 cgroup rw\n"
                    .to_string(),
            ),
            (
                PathBuf::from("/run/cgroup v2/tenant/group/memory.current"),
                (80 * BYTES_PER_MIB).to_string(),
            ),
            (
                PathBuf::from("/proc/self/smaps_rollup"),
                "Pss: 10 kB\n".to_string(),
            ),
        ]);
        let sample =
            sample_linux_memory_usage_with(|path| files.get(path).cloned()).expect("cgroup sample");
        assert_eq!(sample.source, MemoryUsageSource::CgroupV2);
        assert_eq!(sample.bytes, 80 * BYTES_PER_MIB);

        let budget = budget(Some(100), MemoryBudgetEnforcement::Fail, 0);
        budget.set_memory_usage_for_test(sample);
        assert!(budget.try_reserve("over", "diskann3", 21).is_err());

        for invalid in [
            "29 23 0:26 /other.slice /run/cgroup rw - cgroup2 cgroup rw\n",
            "29 23 0:26 /workload.slice /run/cgroup rw - cgroup2 cgroup rw\n",
        ] {
            let files = HashMap::from([
                (
                    PathBuf::from("/proc/self/cgroup"),
                    "0::/workload.slice/tenant/group\n".to_string(),
                ),
                (PathBuf::from("/proc/self/mountinfo"), invalid.to_string()),
                (
                    PathBuf::from("/run/cgroup/tenant/group/memory.current"),
                    "not-a-number\n".to_string(),
                ),
                (
                    PathBuf::from("/proc/self/smaps_rollup"),
                    "Rss: 10240 kB\n".to_string(),
                ),
            ]);
            let sample = sample_linux_memory_usage_with(|path| files.get(path).cloned())
                .expect("RSS fallback");
            assert_eq!(sample.source, MemoryUsageSource::RssFallback);
            assert_eq!(sample.bytes, 10 * BYTES_PER_MIB);
        }

        budget.update_memory_usage(None);
        assert_eq!(budget.used_memory_bytes(), 80 * BYTES_PER_MIB);
        assert_eq!(budget.snapshot().usage_source, MemoryUsageSource::CgroupV2);
    }
}
