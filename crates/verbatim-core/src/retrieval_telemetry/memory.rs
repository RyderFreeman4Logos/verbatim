//! Opaque, bounded cgroup-memory snapshots for retrieval telemetry.

use serde::{Deserialize, Serialize};

use super::{TelemetryDiagnosticCode, TelemetryError, TelemetryResult};

/// Largest representable memory amount in a contract snapshot (one EiB).
pub const MAX_MEMORY_SNAPSHOT_BYTES: u64 = 1 << 60;

/// Counters mirrored from cgroup v2 `memory.events` without a cgroup path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEventCounters {
    pub low: u64,
    pub high: u64,
    pub max: u64,
    pub oom: u64,
    pub oom_kill: u64,
}

/// Construction fields for a validated [`MemorySnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySnapshotFields {
    pub cgroup_current_bytes: u64,
    pub cgroup_peak_bytes: u64,
    pub events: MemoryEventCounters,
    pub anonymous_bytes: u64,
    pub file_bytes: u64,
    pub kernel_bytes: u64,
}

/// A path-free cgroup v2 memory observation with bounded numeric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemorySnapshot {
    cgroup_current_bytes: u64,
    cgroup_peak_bytes: u64,
    events: MemoryEventCounters,
    anonymous_bytes: u64,
    file_bytes: u64,
    kernel_bytes: u64,
}

impl<'de> Deserialize<'de> for MemorySnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = MemorySnapshotFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl MemorySnapshot {
    /// Creates a bounded snapshot without retaining a cgroup name or filesystem path.
    pub fn new(fields: MemorySnapshotFields) -> TelemetryResult<Self> {
        let snapshot = Self {
            cgroup_current_bytes: fields.cgroup_current_bytes,
            cgroup_peak_bytes: fields.cgroup_peak_bytes,
            events: fields.events,
            anonymous_bytes: fields.anonymous_bytes,
            file_bytes: fields.file_bytes,
            kernel_bytes: fields.kernel_bytes,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Revalidates all numeric bounds and snapshot relationships.
    pub fn validate(&self) -> TelemetryResult<()> {
        for amount in [
            self.cgroup_current_bytes,
            self.cgroup_peak_bytes,
            self.anonymous_bytes,
            self.file_bytes,
            self.kernel_bytes,
        ] {
            if amount > MAX_MEMORY_SNAPSHOT_BYTES {
                return Err(TelemetryError::contract(
                    TelemetryDiagnosticCode::MemorySnapshotExceedsBound,
                ));
            }
        }
        if self.cgroup_current_bytes > self.cgroup_peak_bytes
            || self.anonymous_bytes > self.cgroup_current_bytes
            || self.file_bytes > self.cgroup_current_bytes
            || self.kernel_bytes > self.cgroup_current_bytes
        {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidMemorySnapshot,
            ));
        }
        let breakdown_total = self
            .anonymous_bytes
            .checked_add(self.file_bytes)
            .and_then(|total| total.checked_add(self.kernel_bytes))
            .ok_or(TelemetryError::contract(
                TelemetryDiagnosticCode::MemorySnapshotExceedsBound,
            ))?;
        if breakdown_total > MAX_MEMORY_SNAPSHOT_BYTES {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::MemorySnapshotExceedsBound,
            ));
        }
        Ok(())
    }

    /// Returns `memory.current` in bytes, including cgroup-accounted page cache.
    pub const fn cgroup_current_bytes(self) -> u64 {
        self.cgroup_current_bytes
    }

    /// Returns cgroup v2 `memory.peak` in bytes.
    pub const fn cgroup_peak_bytes(self) -> u64 {
        self.cgroup_peak_bytes
    }

    /// Returns path-free cgroup memory-event counters.
    pub const fn events(self) -> MemoryEventCounters {
        self.events
    }

    /// Returns anonymous memory in bytes.
    pub const fn anonymous_bytes(self) -> u64 {
        self.anonymous_bytes
    }

    /// Returns file/page-cache memory in bytes.
    pub const fn file_bytes(self) -> u64 {
        self.file_bytes
    }

    /// Returns kernel-accounted memory in bytes.
    pub const fn kernel_bytes(self) -> u64 {
        self.kernel_bytes
    }

    /// Returns validated fields without adding a cgroup identity or path.
    pub const fn as_fields(self) -> MemorySnapshotFields {
        MemorySnapshotFields {
            cgroup_current_bytes: self.cgroup_current_bytes,
            cgroup_peak_bytes: self.cgroup_peak_bytes,
            events: self.events,
            anonymous_bytes: self.anonymous_bytes,
            file_bytes: self.file_bytes,
            kernel_bytes: self.kernel_bytes,
        }
    }
}
