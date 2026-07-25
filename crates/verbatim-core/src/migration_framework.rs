//! Migration framework contract for schema, config, and artifact versions
//! (MIGRATE-001 / issue #335).
//!
//! Walking skeleton: ordered transactional migrations with checksummed history,
//! forward-version fail-closed detection, preflight (disk + backup + window),
//! artifact versioning, and an explicit read-only adapter for newer stores.
//!
//! Residual: live SQLite wiring, real SQL migrations, config auto-migrate,
//! golden fixtures, multi-service upgrade matrix, closing #335. See
//! `docs/architecture/versioned-migrations.md`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use crate::types::hex_sha256;

/// Wire schema version for framework documents (history, plans, preflight).
/// Unknown versions fail closed on decode.
pub const MIGRATION_FRAMEWORK_SCHEMA_VERSION: u32 = 1;

/// Semantic schema / application version (`major.minor.patch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SchemaVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub fn as_dotted(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// Parse `"major.minor.patch"`; rejects empty / non-numeric parts.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let major = parse_component(parts.next(), "major")?;
        let minor = parse_component(parts.next(), "minor")?;
        let patch = parse_component(parts.next(), "patch")?;
        if parts.next().is_some() {
            bail!("schema version must have exactly three components: {text}");
        }
        Ok(Self::new(major, minor, patch))
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_dotted())
    }
}

fn parse_component(part: Option<&str>, name: &str) -> Result<u32> {
    let Some(raw) = part else {
        bail!("schema version missing {name} component");
    };
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        bail!("schema version {name} component is not a non-negative integer: {raw}");
    }
    raw.parse::<u32>()
        .map_err(|err| anyhow::anyhow!("schema version {name} overflow: {err}"))
}

/// Inclusive compatibility window (`min_supported <= max_supported`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompatibilityWindow {
    pub min_supported: SchemaVersion,
    pub max_supported: SchemaVersion,
}

impl CompatibilityWindow {
    pub fn new(min_supported: SchemaVersion, max_supported: SchemaVersion) -> Result<Self> {
        if min_supported > max_supported {
            bail!(
                "compatibility window min_supported ({min_supported}) exceeds max_supported ({max_supported})"
            );
        }
        Ok(Self {
            min_supported,
            max_supported,
        })
    }

    pub fn contains(&self, version: SchemaVersion) -> bool {
        version >= self.min_supported && version <= self.max_supported
    }

    pub fn classify(&self, version: SchemaVersion) -> VersionRelation {
        if version < self.min_supported {
            VersionRelation::TooOld
        } else if version > self.max_supported {
            VersionRelation::TooNew
        } else {
            VersionRelation::Compatible
        }
    }
}

/// Relation of a store/artifact version to a [`CompatibilityWindow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionRelation {
    Compatible,
    TooOld,
    TooNew,
}

/// Access mode after version detection.
///
/// `ReadOnlyAdapter` is the only escape hatch for stores newer than
/// `max_supported`. It never runs upgrade migrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// Full read/write after successful upgrade (or already current).
    ReadWrite,
    /// Explicit read-only adapter for a newer schema; no mutations.
    ReadOnlyAdapter,
}

/// Kind of versioned persisted artifact (not live-wired in this slice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    QueryPlan,
    EvidencePack,
    ContextPack,
    GraphReport,
    WorkflowRun,
    Cursor,
    TaskProfile,
    PublicationManifest,
    Config,
    IndexMetadata,
}

/// Version stamp for a persisted artifact.
///
/// Immutable historical artifacts are adapted or copied at read time; never
/// semantically rewritten in place by migrations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub kind: ArtifactKind,
    pub version: SchemaVersion,
    /// Content checksum (hex SHA-256) of the canonical artifact bytes.
    pub checksum: String,
    /// When true, migrations must adapt/copy rather than mutate in place.
    pub immutable: bool,
}

impl ArtifactVersion {
    pub fn new(
        kind: ArtifactKind,
        version: SchemaVersion,
        checksum: impl Into<String>,
        immutable: bool,
    ) -> Self {
        Self {
            kind,
            version,
            checksum: checksum.into(),
            immutable,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.checksum.trim().is_empty() {
            bail!("artifact checksum must not be empty for {:?}", self.kind);
        }
        Ok(())
    }
}

/// Ordered migration step descriptor (identity + checksum, not SQL body).
///
/// Ordered by `sequence`. History checksums must match the registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MigrationStep {
    pub id: String,
    pub sequence: u32,
    pub from_version: SchemaVersion,
    pub to_version: SchemaVersion,
    /// Hex SHA-256 of the canonical migration body / transform definition.
    pub checksum: String,
    pub description: String,
    /// Re-applying an already-applied step is a no-op success when true.
    pub idempotent: bool,
    /// Must run inside a single transaction boundary when true.
    pub transactional: bool,
}

impl MigrationStep {
    pub fn new(params: MigrationStepParams) -> Result<Self> {
        if params.id.trim().is_empty() {
            bail!("migration id must not be empty");
        }
        if params.checksum.trim().is_empty() {
            bail!("migration checksum must not be empty for {}", params.id);
        }
        if params.from_version >= params.to_version {
            bail!(
                "migration {} must advance version ({} -> {})",
                params.id,
                params.from_version,
                params.to_version
            );
        }
        Ok(Self {
            id: params.id,
            sequence: params.sequence,
            from_version: params.from_version,
            to_version: params.to_version,
            checksum: params.checksum,
            description: params.description,
            idempotent: params.idempotent,
            transactional: params.transactional,
        })
    }

    pub fn checksum_of(body: &[u8]) -> String {
        hex_sha256(body)
    }
}

/// Field bundle for [`MigrationStep::new`].
#[derive(Debug, Clone)]
pub struct MigrationStepParams {
    pub id: String,
    pub sequence: u32,
    pub from_version: SchemaVersion,
    pub to_version: SchemaVersion,
    pub checksum: String,
    pub description: String,
    pub idempotent: bool,
    pub transactional: bool,
}

/// Outcome recorded for a single applied migration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationApplyStatus {
    Applied,
    SkippedIdempotent,
    Failed,
    Interrupted,
}

/// One row in [`MigrationHistory`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MigrationHistoryEntry {
    pub migration_id: String,
    pub sequence: u32,
    pub from_version: SchemaVersion,
    pub to_version: SchemaVersion,
    /// Checksum recorded at apply time (must match registry).
    pub checksum: String,
    pub status: MigrationApplyStatus,
    /// Monotonic apply counter for tests / recovery (not wall clock).
    pub applied_at_counter: u64,
}

fn status_is_success(status: MigrationApplyStatus) -> bool {
    matches!(
        status,
        MigrationApplyStatus::Applied | MigrationApplyStatus::SkippedIdempotent
    )
}

/// Append-only style history of applied migrations with checksums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MigrationHistory {
    pub schema_version: u32,
    pub entries: Vec<MigrationHistoryEntry>,
}

impl MigrationHistory {
    pub fn new() -> Self {
        Self {
            schema_version: MIGRATION_FRAMEWORK_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_framework_schema_version(self.schema_version)
    }

    pub fn applied_ids(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| status_is_success(e.status))
            .map(|e| e.migration_id.as_str())
            .collect()
    }

    pub fn last_completed_version(&self) -> Option<SchemaVersion> {
        self.entries
            .iter()
            .rev()
            .find(|e| status_is_success(e.status))
            .map(|e| e.to_version)
    }

    pub fn has_unrecovered_interruption(&self) -> bool {
        matches!(
            self.entries.last().map(|e| e.status),
            Some(MigrationApplyStatus::Interrupted | MigrationApplyStatus::Failed)
        )
    }

    pub fn recorded_checksum(&self, migration_id: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.migration_id == migration_id)
            .map(|e| e.checksum.as_str())
    }
}

/// Disk-space requirement reported by preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DiskSpaceRequirement {
    pub available_bytes: u64,
    pub required_bytes: u64,
}

impl DiskSpaceRequirement {
    pub fn validate(&self) -> Result<()> {
        if self.available_bytes < self.required_bytes {
            bail!(
                "insufficient disk space for migration: available {} bytes, required {} bytes",
                self.available_bytes,
                self.required_bytes
            );
        }
        Ok(())
    }
}

/// Backup hook contract invoked before destructive migrations.
pub trait BackupHook {
    fn create_backup(&mut self, label: &str) -> Result<String>;
}

/// No-op backup hook for pure unit tests.
#[derive(Debug, Default)]
pub struct NoopBackupHook {
    pub calls: Vec<String>,
}

impl BackupHook for NoopBackupHook {
    fn create_backup(&mut self, label: &str) -> Result<String> {
        self.calls.push(label.to_string());
        Ok(format!("noop-backup:{label}"))
    }
}

/// Recording backup hook that can be configured to fail.
#[derive(Debug, Default)]
pub struct RecordingBackupHook {
    pub calls: Vec<String>,
    pub fail: bool,
}

impl BackupHook for RecordingBackupHook {
    fn create_backup(&mut self, label: &str) -> Result<String> {
        self.calls.push(label.to_string());
        if self.fail {
            bail!("backup hook failed for label {label}");
        }
        Ok(format!("backup:{label}"))
    }
}

/// Preflight outcome before mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub schema_version: u32,
    pub current_version: SchemaVersion,
    pub target_version: SchemaVersion,
    pub window: CompatibilityWindow,
    pub disk: DiskSpaceRequirement,
    pub backup_created: bool,
    pub backup_id: Option<String>,
}

impl PreflightReport {
    pub fn validate_schema(&self) -> Result<()> {
        validate_framework_schema_version(self.schema_version)
    }
}

/// Decision returned by version detection before any mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VersionDecision {
    UpToDate {
        version: SchemaVersion,
        access: AccessMode,
    },
    NeedsUpgrade {
        from: SchemaVersion,
        to: SchemaVersion,
        steps: Vec<String>,
        access: AccessMode,
    },
    NewerThanSupported {
        store_version: SchemaVersion,
        max_supported: SchemaVersion,
        access: AccessMode,
    },
    OlderThanSupported {
        store_version: SchemaVersion,
        min_supported: SchemaVersion,
    },
}

/// In-memory migration registry + planner (no live SQLite).
#[derive(Debug, Clone)]
pub struct MigrationFramework {
    pub schema_version: u32,
    pub application_id: u32,
    pub window: CompatibilityWindow,
    steps: BTreeMap<String, MigrationStep>,
}

impl MigrationFramework {
    pub fn new(application_id: u32, window: CompatibilityWindow) -> Self {
        Self {
            schema_version: MIGRATION_FRAMEWORK_SCHEMA_VERSION,
            application_id,
            window,
            steps: BTreeMap::new(),
        }
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_framework_schema_version(self.schema_version)
    }

    pub fn register(&mut self, step: MigrationStep) -> Result<()> {
        if self.steps.contains_key(&step.id) {
            bail!("duplicate migration id: {}", step.id);
        }
        self.steps.insert(step.id.clone(), step);
        Ok(())
    }

    pub fn ordered_steps(&self) -> Vec<&MigrationStep> {
        let mut steps: Vec<&MigrationStep> = self.steps.values().collect();
        steps.sort_by(|a, b| a.sequence.cmp(&b.sequence).then_with(|| a.id.cmp(&b.id)));
        steps
    }

    /// Detect action for a store at `current` targeting `window.max_supported`.
    ///
    /// `allow_read_only_adapter` must be explicit for newer-than-supported stores.
    pub fn detect_version(
        &self,
        current: SchemaVersion,
        allow_read_only_adapter: bool,
    ) -> Result<VersionDecision> {
        let target = self.window.max_supported;
        match self.window.classify(current) {
            VersionRelation::TooOld => Ok(VersionDecision::OlderThanSupported {
                store_version: current,
                min_supported: self.window.min_supported,
            }),
            VersionRelation::TooNew => {
                if allow_read_only_adapter {
                    Ok(VersionDecision::NewerThanSupported {
                        store_version: current,
                        max_supported: target,
                        access: AccessMode::ReadOnlyAdapter,
                    })
                } else {
                    bail!(
                        "store schema version {current} is newer than supported max {target}; refuse write access without explicit read-only adapter"
                    );
                }
            }
            VersionRelation::Compatible if current == target => Ok(VersionDecision::UpToDate {
                version: current,
                access: AccessMode::ReadWrite,
            }),
            VersionRelation::Compatible if current > target => {
                bail!("internal: current {current} > target {target} but classified compatible")
            }
            VersionRelation::Compatible => {
                let plan = self.plan_upgrade(current, target)?;
                Ok(VersionDecision::NeedsUpgrade {
                    from: current,
                    to: target,
                    steps: plan.iter().map(|s| s.id.clone()).collect(),
                    access: AccessMode::ReadWrite,
                })
            }
        }
    }

    /// Ordered upgrade plan from `from` to `to`.
    pub fn plan_upgrade(
        &self,
        from: SchemaVersion,
        to: SchemaVersion,
    ) -> Result<Vec<MigrationStep>> {
        if from > to {
            bail!("cannot plan upgrade from newer {from} to older {to}");
        }
        if from == to {
            return Ok(Vec::new());
        }
        let mut plan = Vec::new();
        let mut cursor = from;
        for step in self.ordered_steps() {
            if step.from_version == cursor && step.to_version <= to {
                plan.push(step.clone());
                cursor = step.to_version;
                if cursor == to {
                    break;
                }
            }
        }
        if cursor != to {
            bail!(
                "incomplete migration path from {from} to {to}; reached {cursor} (missing steps)"
            );
        }
        Ok(plan)
    }

    /// Validate history checksums against the registry (corrupt history fails closed).
    pub fn verify_history_checksums(&self, history: &MigrationHistory) -> Result<()> {
        history.validate_schema()?;
        for entry in &history.entries {
            if !status_is_success(entry.status) {
                continue;
            }
            let Some(step) = self.steps.get(&entry.migration_id) else {
                bail!(
                    "history references unknown migration id {}",
                    entry.migration_id
                );
            };
            if step.checksum != entry.checksum {
                bail!(
                    "migration checksum mismatch for {}: history has {}, registry has {}",
                    entry.migration_id,
                    entry.checksum,
                    step.checksum
                );
            }
        }
        Ok(())
    }

    /// Preflight: window, disk space, optional backup hook, history integrity.
    pub fn preflight(
        &self,
        current: SchemaVersion,
        target: SchemaVersion,
        disk: DiskSpaceRequirement,
        history: &MigrationHistory,
        backup: Option<&mut dyn BackupHook>,
        backup_label: &str,
    ) -> Result<PreflightReport> {
        self.validate_schema()?;
        history.validate_schema()?;
        self.verify_history_checksums(history)?;
        if history.has_unrecovered_interruption() {
            bail!(
                "unrecovered interrupted or failed migration present in history; recover before starting a new migration"
            );
        }
        match self.window.classify(current) {
            VersionRelation::TooOld => bail!(
                "current schema {current} is older than min_supported {}; offline upgrade required",
                self.window.min_supported
            ),
            VersionRelation::TooNew => bail!(
                "current schema {current} is newer than max_supported {}; cannot migrate forward",
                self.window.max_supported
            ),
            VersionRelation::Compatible => {}
        }
        if !self.window.contains(target) {
            bail!(
                "target schema {target} is outside compatibility window [{}, {}]",
                self.window.min_supported,
                self.window.max_supported
            );
        }
        if target < current {
            bail!("downgrade from {current} to {target} is not supported by this framework");
        }
        disk.validate()?;
        let (backup_created, backup_id) = if let Some(hook) = backup {
            (true, Some(hook.create_backup(backup_label)?))
        } else {
            (false, None)
        };
        if current != target {
            let _plan = self.plan_upgrade(current, target)?;
        }
        Ok(PreflightReport {
            schema_version: MIGRATION_FRAMEWORK_SCHEMA_VERSION,
            current_version: current,
            target_version: target,
            window: self.window,
            disk,
            backup_created,
            backup_id,
        })
    }

    /// Apply pending migrations against an in-memory logical version + history.
    ///
    /// `interrupt_after` forces the Nth runnable step into `Interrupted` and
    /// stops. Recovery drops the unrecovered tail, then re-apply.
    pub fn apply_pending(
        &self,
        current: SchemaVersion,
        target: SchemaVersion,
        history: &mut MigrationHistory,
        apply_counter: &mut u64,
        interrupt_after: Option<usize>,
    ) -> Result<SchemaVersion> {
        self.verify_history_checksums(history)?;
        if history.has_unrecovered_interruption() {
            bail!("cannot apply while history has unrecovered interruption/failure");
        }
        let plan = self.plan_upgrade(current, target)?;
        let mut version = current;
        let mut runnable_index = 0usize;
        for step in plan {
            let already = history.entries.iter().any(|e| {
                e.migration_id == step.id
                    && e.checksum == step.checksum
                    && status_is_success(e.status)
            });
            if already {
                if !step.idempotent {
                    bail!(
                        "migration {} already applied and is not marked idempotent",
                        step.id
                    );
                }
                self.push_history(
                    history,
                    &step,
                    MigrationApplyStatus::SkippedIdempotent,
                    apply_counter,
                );
                version = step.to_version;
                continue;
            }
            if let Some(recorded) = history.recorded_checksum(&step.id) {
                if recorded != step.checksum {
                    bail!(
                        "migration checksum mismatch for {}: history has {}, registry has {}",
                        step.id,
                        recorded,
                        step.checksum
                    );
                }
            }
            if interrupt_after == Some(runnable_index) {
                self.push_history(
                    history,
                    &step,
                    MigrationApplyStatus::Interrupted,
                    apply_counter,
                );
                bail!(
                    "migration {} interrupted before commit; prior state is recoverable",
                    step.id
                );
            }
            if !step.transactional {
                bail!(
                    "non-transactional migration {} is not supported by this contract",
                    step.id
                );
            }
            self.push_history(history, &step, MigrationApplyStatus::Applied, apply_counter);
            version = step.to_version;
            runnable_index = runnable_index.saturating_add(1);
        }
        Ok(version)
    }

    fn push_history(
        &self,
        history: &mut MigrationHistory,
        step: &MigrationStep,
        status: MigrationApplyStatus,
        apply_counter: &mut u64,
    ) {
        *apply_counter = apply_counter.saturating_add(1);
        history.entries.push(MigrationHistoryEntry {
            migration_id: step.id.clone(),
            sequence: step.sequence,
            from_version: step.from_version,
            to_version: step.to_version,
            checksum: step.checksum.clone(),
            status,
            applied_at_counter: *apply_counter,
        });
    }

    /// Recover from an interrupted/failed last entry by dropping it (rollback).
    pub fn recover_interrupted(history: &mut MigrationHistory) -> Result<()> {
        history.validate_schema()?;
        match history.entries.last().map(|e| e.status) {
            Some(MigrationApplyStatus::Interrupted | MigrationApplyStatus::Failed) => {
                history.entries.pop();
                Ok(())
            }
            Some(_) => bail!("no interrupted/failed migration at history tail to recover"),
            None => bail!("empty history; nothing to recover"),
        }
    }

    /// Classify an artifact version against the framework window.
    ///
    /// Newer artifacts require an explicit read-only adapter; immutable packs
    /// are never rewritten in place.
    pub fn classify_artifact(
        &self,
        artifact: &ArtifactVersion,
        allow_read_only_adapter: bool,
    ) -> Result<AccessMode> {
        artifact.validate()?;
        match self.window.classify(artifact.version) {
            VersionRelation::Compatible => Ok(AccessMode::ReadWrite),
            VersionRelation::TooOld => bail!(
                "artifact {:?} version {} is older than min_supported {}",
                artifact.kind,
                artifact.version,
                self.window.min_supported
            ),
            VersionRelation::TooNew if allow_read_only_adapter => {
                // Immutable and mutable newer artifacts both require the adapter;
                // immutable ones are adapted/copied, never rewritten in place.
                let _ = artifact.immutable;
                Ok(AccessMode::ReadOnlyAdapter)
            }
            VersionRelation::TooNew => bail!(
                "artifact {:?} version {} is newer than max_supported {}; refuse without read-only adapter",
                artifact.kind,
                artifact.version,
                self.window.max_supported
            ),
        }
    }
}

/// Decode [`MigrationHistory`] and reject unknown schema versions.
pub fn decode_migration_history_json(raw: &str) -> Result<MigrationHistory> {
    let history: MigrationHistory = serde_json::from_str(raw)?;
    history.validate_schema()?;
    Ok(history)
}

/// Decode [`PreflightReport`] and reject unknown schema versions.
pub fn decode_preflight_report_json(raw: &str) -> Result<PreflightReport> {
    let report: PreflightReport = serde_json::from_str(raw)?;
    report.validate_schema()?;
    Ok(report)
}

fn validate_framework_schema_version(schema_version: u32) -> Result<()> {
    if schema_version != MIGRATION_FRAMEWORK_SCHEMA_VERSION {
        bail!(
            "unsupported migration framework schema version {schema_version}; expected {MIGRATION_FRAMEWORK_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "migration_framework_tests.rs"]
mod tests;
