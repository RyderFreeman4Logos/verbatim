//! Process-isolation spec: separate build/compaction from online serving.
//!
//! Issue #377 requires independent cgroups and CPU/I/O priorities for build vs
//! serving, and deterministic shutdown that cancels I/O and releases mapped
//! files/caches. This module declares the validated isolation spec — the
//! cgroup path, CPU/IO priority classes, and worker separation flag. It does
//! **not** spawn processes, write cgroup files, or call `setpriority`.
//!
//! Contract only.

use serde::{Deserialize, Serialize};

use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// Linux CPU scheduling class for an isolated process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuPriorityClass {
    /// Best-effort, lower priority than serving (build/compaction).
    BestEffort,
    /// Normal priority (serving default).
    Normal,
    /// Realtime-ish / elevated (reserved for critical serving paths).
    Elevated,
}

impl CpuPriorityClass {
    pub const ALL: [Self; 3] = [Self::BestEffort, Self::Normal, Self::Elevated];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BestEffort => "best_effort",
            Self::Normal => "normal",
            Self::Elevated => "elevated",
        }
    }
}

/// Linux I/O scheduling class (`ionice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoPriorityClass {
    /// `ionice -c 3`: no real-time guarantee (build/compaction).
    Idle,
    /// `ionice -c 2`: best-effort with configurable level.
    BestEffort,
    /// `ionice -c 1`: realtime (reserved for critical serving paths).
    Realtime,
}

impl IoPriorityClass {
    pub const ALL: [Self; 3] = [Self::Idle, Self::BestEffort, Self::Realtime];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::BestEffort => "best_effort",
            Self::Realtime => "realtime",
        }
    }
}

/// Field bag used to construct and validate a [`ProcessIsolationSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIsolationSpecFields {
    /// Opaque numeric ID distinguishing this cgroup slice. The actual cgroup
    /// path is constructed by the adapter from this ID plus a fixed prefix; the
    /// contract never stores a caller-controlled path string.
    pub cgroup_slice_id: u32,
    /// CPU scheduling class.
    pub cpu_priority: CpuPriorityClass,
    /// I/O scheduling class.
    pub io_priority: IoPriorityClass,
    /// Whether this process owns its own worker pool (true) or shares the
    /// serving pool (false). Build/compaction must be `true`.
    pub dedicated_workers: bool,
}

/// A validated process-isolation specification.
///
/// The cgroup slice ID must be positive (zero is reserved for the unset
/// sentinel). Build/compaction processes must declare `dedicated_workers = true`
/// so they cannot evict the online serving working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIsolationSpec {
    fields: ProcessIsolationSpecFields,
}

impl ProcessIsolationSpec {
    /// Constructs a spec only when the cgroup slice ID is positive.
    pub fn new(fields: ProcessIsolationSpecFields) -> RetrievalBudgetResult<Self> {
        let spec = Self { fields };
        spec.validate()?;
        Ok(spec)
    }

    /// Revalidates invariants after decode or before an adapter launches a process.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        if self.fields.cgroup_slice_id == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidProcessIsolation,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> ProcessIsolationSpecFields {
        self.fields
    }

    /// Returns `true` when this spec isolates a build/compaction process
    /// (dedicated workers and best-effort/low CPU+IO priority).
    pub const fn is_build_isolated(&self) -> bool {
        self.fields.dedicated_workers
            && matches!(self.fields.cpu_priority, CpuPriorityClass::BestEffort)
            && matches!(self.fields.io_priority, IoPriorityClass::Idle)
    }

    /// Conservative walking-skeleton default for online serving.
    pub const fn skeleton_online_serving() -> Self {
        Self {
            fields: ProcessIsolationSpecFields {
                cgroup_slice_id: 1,
                cpu_priority: CpuPriorityClass::Normal,
                io_priority: IoPriorityClass::BestEffort,
                dedicated_workers: false,
            },
        }
    }

    /// Conservative walking-skeleton default for isolated build/compaction.
    pub const fn skeleton_isolated_build() -> Self {
        Self {
            fields: ProcessIsolationSpecFields {
                cgroup_slice_id: 2,
                cpu_priority: CpuPriorityClass::BestEffort,
                io_priority: IoPriorityClass::Idle,
                dedicated_workers: true,
            },
        }
    }
}
