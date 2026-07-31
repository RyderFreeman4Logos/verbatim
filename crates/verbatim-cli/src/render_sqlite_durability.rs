//! SQLite durability health rendering for compact and verbose CLI output.
//!
//! The daemon `/health` endpoint optionally carries the effective SQLite
//! durability profile, PRAGMAs, WAL size, RPO, and disk headroom. These helpers
//! keep that rendering cohesive and leave the main health writer focused on
//! readiness, memory, and resource lines.

use std::io::Write;

use verbatim_core::api::HealthResponse;
use verbatim_core::store::SqliteDurabilityStatus;

/// Append the compact one-line SQLite durability summary (`sqlite=… journal=…
/// sync=… wal_bytes=…`) when the daemon reported durability state.
pub(super) fn write_compact<W: Write>(
    writer: &mut W,
    health: &HealthResponse,
) -> std::io::Result<()> {
    if let Some(sqlite) = &health.sqlite_durability {
        write!(
            writer,
            " sqlite={} journal={} sync={} wal_bytes={}",
            sqlite.effective.profile,
            sqlite.effective.journal_mode,
            sqlite.effective.synchronous,
            sqlite.wal_bytes,
        )?;
    }
    Ok(())
}

/// Write the multi-line verbose SQLite durability block (profile, PRAGMAs, RPO,
/// and optional disk headroom) when the daemon reported durability state.
pub(super) fn write_verbose<W: Write>(
    writer: &mut W,
    health: &HealthResponse,
) -> std::io::Result<()> {
    if let Some(sqlite) = &health.sqlite_durability {
        write_verbose_block(writer, sqlite)?;
    }
    Ok(())
}

fn write_verbose_block<W: Write>(
    writer: &mut W,
    sqlite: &SqliteDurabilityStatus,
) -> std::io::Result<()> {
    let effective = &sqlite.effective;
    writeln!(
        writer,
        "SQLite durability: profile={} journal={} synchronous={} busy_timeout_ms={} wal_autocheckpoint_pages={} checkpoint={:?} wal_bytes={} wal_alert_bytes={}",
        effective.profile,
        effective.journal_mode,
        effective.synchronous,
        effective.busy_timeout_millis,
        effective.wal_autocheckpoint_pages,
        effective.checkpoint_mode,
        sqlite.wal_bytes,
        effective.wal_alert_bytes,
    )?;
    writeln!(writer, "SQLite RPO: {}", effective.rpo)?;
    if let Some(disk) = sqlite.disk {
        writeln!(
            writer,
            "SQLite disk: available={} reserve={} below_reserve={}",
            disk.available_bytes, disk.reserve_bytes, disk.below_reserve
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use verbatim_core::api::{HealthResponse, ReadinessHealth};

    #[test]
    fn health_compact_shows_memory_source_and_idle_flags() {
        use verbatim_core::api::{IdleExitHealth, IdleReclaimHealth};
        use verbatim_core::memory_budget::{MemoryBudgetSnapshot, MemoryUsageSource};
        let health = HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::ready(),
            memory_budget: MemoryBudgetSnapshot {
                rss_mb: 123,
                used_memory_mb: 282,
                usage_source: MemoryUsageSource::CgroupV2,
                ..Default::default()
            },
            resources: Vec::new(),
            idle_reclaim: Some(IdleReclaimHealth {
                enabled: true,
                sqlite_shrink_memory: true,
                malloc_trim: true,
                currently_idle: true,
                eligible: false,
                skip_reason: None,
                idle_for_millis: 30_000,
                idle_timeout_millis: 300_000,
                min_interval_millis: 900_000,
                next_eligible_in_millis: None,
                active: Default::default(),
                last_result: None,
                last_attempt_result: None,
            }),
            idle_exit: Some(IdleExitHealth {
                enabled: true,
                count_health_requests: false,
                allow_with_collection_watcher: true,
                auto_start_on_cli: true,
                currently_idle: false,
                eligible: false,
                skip_reason: Some("active_tasks".into()),
                idle_for_millis: 0,
                timeout_millis: 1_200_000,
                last_activity_unix_ms: 0,
                deadline_unix_ms: 0,
                next_eligible_in_millis: None,
                active: Default::default(),
            }),
            sqlite_durability: None,
        };
        let mut output = Vec::new();
        write_health_compact(&mut output, &health).unwrap();
        let out = String::from_utf8(output).unwrap();
        assert!(out.contains("ok memory=282MB(cgroup_v2_memory_current) rss=123MB"));
        assert!(out.contains("idle_reclaim=enabled"));
        assert!(out.contains("idle_exit=enabled(1200s)"));
        assert!(out.contains("--details"));
    }

    #[test]
    fn health_compact_minimal_when_no_idle_features() {
        use verbatim_core::memory_budget::{MemoryBudgetSnapshot, MemoryUsageSource};
        let health = HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::ready(),
            memory_budget: MemoryBudgetSnapshot {
                rss_mb: 100,
                used_memory_mb: 100,
                usage_source: MemoryUsageSource::RssFallback,
                ..Default::default()
            },
            resources: Vec::new(),
            idle_reclaim: None,
            idle_exit: None,
            sqlite_durability: None,
        };
        let mut output = Vec::new();
        write_health_compact(&mut output, &health).unwrap();
        let out = String::from_utf8(output).unwrap();
        assert!(out.contains("ok memory=100MB(rss_fallback) rss=100MB"));
        assert!(!out.contains("idle_reclaim"));
        assert!(!out.contains("idle_exit"));
    }
}
