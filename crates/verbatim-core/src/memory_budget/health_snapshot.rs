use serde::{Deserialize, Serialize};

use crate::config::MemoryBudgetEnforcement;

/// Source of the memory measurement used for admission decisions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryUsageSource {
    /// Linux cgroup v2 `memory.current`, including charged file page cache.
    CgroupV2,
    /// Process RSS used only when cgroup v2 accounting is unavailable or invalid.
    RssFallback,
    /// Neither cgroup v2 nor platform RSS accounting was available.
    #[default]
    Unavailable,
}

impl MemoryUsageSource {
    /// Stable label emitted by health and pressure diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CgroupV2 => "cgroup_v2_memory_current",
            Self::RssFallback => "rss_fallback",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryBudgetSnapshot {
    pub limit_mb: Option<usize>,
    /// Process resident set size, independent of admission usage.
    pub rss_mb: u64,
    /// Measured bytes used for admission, expressed in MiB.
    pub used_memory_mb: u64,
    /// Identifies whether admission included cgroup page cache or fell back to RSS.
    pub usage_source: MemoryUsageSource,
    pub reserved_mb: usize,
    pub available_mb: Option<usize>,
    pub enforcement: MemoryBudgetEnforcement,
    pub active_reservations: Vec<MemoryReservationSnapshot>,
}

#[derive(Deserialize)]
struct MemoryBudgetSnapshotWire {
    limit_mb: Option<usize>,
    rss_mb: u64,
    used_memory_mb: Option<u64>,
    usage_source: Option<MemoryUsageSource>,
    reserved_mb: usize,
    available_mb: Option<usize>,
    enforcement: MemoryBudgetEnforcement,
    active_reservations: Vec<MemoryReservationSnapshot>,
}

impl<'de> Deserialize<'de> for MemoryBudgetSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = MemoryBudgetSnapshotWire::deserialize(deserializer)?;
        let (used_memory_mb, usage_source) = match (wire.used_memory_mb, wire.usage_source) {
            (Some(used_memory_mb), Some(usage_source)) => (used_memory_mb, usage_source),
            (Some(used_memory_mb), None) => (used_memory_mb, MemoryUsageSource::Unavailable),
            (None, _) => (wire.rss_mb, MemoryUsageSource::RssFallback),
        };
        Ok(Self {
            limit_mb: wire.limit_mb,
            rss_mb: wire.rss_mb,
            used_memory_mb,
            usage_source,
            reserved_mb: wire.reserved_mb,
            available_mb: wire.available_mb,
            enforcement: wire.enforcement,
            active_reservations: wire.active_reservations,
        })
    }
}

impl Default for MemoryBudgetSnapshot {
    fn default() -> Self {
        Self {
            limit_mb: None,
            rss_mb: 0,
            used_memory_mb: 0,
            usage_source: MemoryUsageSource::Unavailable,
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

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_DAEMON_HEALTH_FIXTURE: &str = r#"{
        "status": "ok",
        "memory_budget": {
            "limit_mb": 256,
            "rss_mb": 41,
            "reserved_mb": 7,
            "available_mb": 208,
            "enforcement": "slow_warn",
            "active_reservations": []
        }
    }"#;

    const NEW_DAEMON_HEALTH_FIXTURE: &str = r#"{
        "status": "ok",
        "process_alive": true,
        "readiness": "ready",
        "retrieval_ready": true,
        "startup_phase": "ready",
        "memory_budget": {
            "limit_mb": null,
            "rss_mb": 64,
            "used_memory_mb": 96,
            "usage_source": "cgroup_v2",
            "reserved_mb": 0,
            "available_mb": null,
            "enforcement": "slow_warn",
            "active_reservations": []
        }
    }"#;

    #[test]
    fn new_cli_decodes_old_daemon_health_fixture() {
        let health: crate::api::HealthResponse =
            serde_json::from_str(OLD_DAEMON_HEALTH_FIXTURE).unwrap();
        let snapshot = health.memory_budget;

        assert_eq!(snapshot.rss_mb, 41);
        assert_eq!(snapshot.used_memory_mb, 41);
        assert_eq!(snapshot.usage_source, MemoryUsageSource::RssFallback);
    }

    #[test]
    fn old_cli_schema_decodes_new_daemon_health_fixture() {
        #[derive(Deserialize)]
        struct OldHealthResponse {
            status: String,
            memory_budget: OldMemoryBudgetSnapshot,
        }

        #[derive(Deserialize)]
        struct OldMemoryBudgetSnapshot {
            rss_mb: u64,
        }

        let new_health = crate::api::HealthResponse {
            status: "ok".into(),
            readiness: crate::api::ReadinessHealth::ready(),
            memory_budget: MemoryBudgetSnapshot {
                rss_mb: 64,
                used_memory_mb: 96,
                usage_source: MemoryUsageSource::CgroupV2,
                ..Default::default()
            },
            resources: Vec::new(),
            idle_reclaim: None,
            idle_exit: None,
            sqlite_durability: None,
        };
        let new_wire = serde_json::to_value(new_health).unwrap();
        let fixture: serde_json::Value = serde_json::from_str(NEW_DAEMON_HEALTH_FIXTURE).unwrap();
        assert_eq!(new_wire, fixture);

        let old: OldHealthResponse = serde_json::from_value(new_wire).unwrap();

        assert_eq!(old.status, "ok");
        assert_eq!(old.memory_budget.rss_mb, 64);
    }
}
