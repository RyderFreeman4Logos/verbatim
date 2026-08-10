//! Validation and atomic persistence for explicit, identity-preserving source relocation.
//!
//! Parser output is path-keyed, while the catalog identity must remain stable across a
//! relocation. This module owns the one fail-closed remap seam and the one SQLite
//! transaction that updates location-bearing catalog fields.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::{
    map_storage_error, row_to_evidence_unit, status_to_str, str_to_status, SqliteWriteOperation,
    Store,
};
use crate::ingest::IngestPipeline;
use crate::parser;
use crate::resource::{global_resource_registry, ResourceLimitConfig};
use crate::traits::EmbeddingClient;
use crate::types::{
    EvidenceId, EvidenceKind, EvidenceUnit, GraphNodeId, GraphNodeKind, Source, SourceId,
    SourceLocator, SourceStatus,
};

#[path = "source_relocation/collection_boundary.rs"]
mod collection_boundary;
#[path = "source_relocation/held_snapshot.rs"]
mod held_snapshot;

use collection_boundary::collection_covering_path;
use held_snapshot::{open_relocation_target, relocation_target_io_error, HeldInputSnapshot};

/// Stable daemon-facing classification for expected source relocation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRelocationErrorKind {
    /// The request conflicts with the catalog or accepted filesystem snapshot.
    Validation,
    /// The requested source identity is not present in the catalog.
    NotFound,
}

#[derive(Debug)]
struct SourceRelocationError {
    kind: SourceRelocationErrorKind,
    source: anyhow::Error,
}

impl fmt::Display for SourceRelocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for SourceRelocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Return the typed client-actionable relocation category in an error chain.
pub fn source_relocation_error_kind(error: &anyhow::Error) -> Option<SourceRelocationErrorKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<SourceRelocationError>()
            .map(|typed| typed.kind)
    })
}

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    /// Record an external, content-identical file relocation without changing catalog identity.
    pub fn relocate_source(&mut self, source_id: &SourceId, new_path: &Path) -> Result<Source> {
        let operation = (|| {
            let source = self.store().get_source(source_id)?.ok_or_else(|| {
                relocation_error(
                    SourceRelocationErrorKind::NotFound,
                    anyhow::anyhow!("source not found: {}", bounded_text(&source_id.0)),
                )
            })?;
            if source.status != SourceStatus::Indexed {
                return Err(validation_message("source is not indexed"));
            }
            let Some(parser_used) = source.parser_used.as_deref() else {
                return Err(validation_message("source has no previous parser"));
            };
            if !path_entry_is_missing(&source.path)? {
                return Err(validation_message(format!(
                    "stored source path still exists: {}",
                    bounded_path(&source.path)
                )));
            }

            let target_file = open_relocation_target(new_path)?;
            let canonical_path = fs::canonicalize(new_path).map_err(|error| {
                relocation_target_io_error("resolve relocation target", new_path, error)
            })?;
            if canonical_path.to_str().is_none() {
                return Err(validation_message(format!(
                    "relocation target is not UTF-8: {}",
                    bounded_path(&canonical_path)
                )));
            }
            let target = HeldInputSnapshot::new(target_file, canonical_path.clone())?;
            target.validate_path_binding(&canonical_path)?;
            if let Some(conflict) = self.store().get_source_by_path(&canonical_path)? {
                if conflict.id != *source_id {
                    return Err(validation_message(format!(
                        "relocation target belongs to source {}: {}",
                        bounded_text(&conflict.id.0),
                        bounded_path(&canonical_path)
                    )));
                }
            }

            let parser = parser::select_parser(parser_used).map_err(validation_error)?;
            let extension = canonical_path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default();
            if !parser
                .supported_extensions()
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(extension))
            {
                return Err(validation_message(format!(
                    "recorded relocation parser {} does not support extension .{}",
                    bounded_text(parser_used),
                    bounded_text(extension)
                )));
            }
            if target.identity.content_sha256 != source.hash {
                return Err(validation_message(
                    "relocation target content hash differs from stored source hash",
                ));
            }
            let parser_path = target.parser_path();
            #[cfg(test)]
            if let Some(hook) = self
                .store()
                .source_relocation_before_parse_hook
                .borrow_mut()
                .take()
            {
                hook();
            }
            let parser_source_id = SourceId::from_path(&parser_path);
            let parsed = parser.parse(&parser_path);
            #[cfg(test)]
            if let Some(hook) = self
                .store()
                .source_relocation_after_parse_hook
                .borrow_mut()
                .take()
            {
                hook();
            }
            let mut parsed = parsed?;
            crate::pdf_selector::attach_pdf_selectors(&mut parsed, &source.hash, parser_used);
            let mut parsed = remap_parser_evidence_identity(parsed, &parser_source_id, source_id)
                .map_err(validation_error)?;
            rewrite_relocation_locator_paths(&mut parsed, &parser_path, &canonical_path)
                .map_err(validation_error)?;
            target.validate_content_identity(&source.hash)?;
            target.validate_path_binding(&canonical_path)?;

            let stored = self
                .store()
                .list_evidence_by_source(source_id)?
                .into_iter()
                .filter(|unit| unit.kind == EvidenceKind::Text)
                .collect::<Vec<_>>();
            validate_relocation_evidence(&stored, &parsed).map_err(validation_error)?;
            self.store()
                .relocate_source(&source, &canonical_path, &target, &stored, &parsed)
        })();
        operation.with_context(|| {
            format!(
                "relocate source {} to {}",
                bounded_text(&source_id.0),
                bounded_path(new_path)
            )
        })
    }
}

impl Store {
    /// Return the source whose stored canonical path exactly matches `path`.
    pub fn get_source_by_path(&self, path: &Path) -> Result<Option<Source>> {
        let path = path
            .to_str()
            .with_context(|| format!("source path is not UTF-8: {}", bounded_path(path)))?;
        self.connection()
            .query_row(
                "SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources WHERE path = ?1",
                params![path],
                row_to_source,
            )
            .optional()
            .map_err(Into::into)
    }

    fn relocate_source(
        &self,
        expected_source: &Source,
        new_path: &Path,
        expected_target: &HeldInputSnapshot,
        expected_evidence: &[EvidenceUnit],
        relocated_evidence: &[EvidenceUnit],
    ) -> Result<Source> {
        if expected_evidence.len() != relocated_evidence.len()
            || expected_evidence
                .iter()
                .zip(relocated_evidence)
                .any(|(expected, relocated)| !evidence_matches_except_locator(expected, relocated))
        {
            return Err(validation_message(format!(
                "relocation evidence changed for source {}",
                bounded_text(&expected_source.id.0)
            )));
        }

        let old_path = expected_source.path.to_str().with_context(|| {
            format!(
                "stored source path is not UTF-8: {}",
                bounded_path(&expected_source.path)
            )
        })?;
        let new_path_text = new_path
            .to_str()
            .with_context(|| format!("relocation path is not UTF-8: {}", bounded_path(new_path)))?;
        let relocated_source = Source {
            id: expected_source.id.clone(),
            path: new_path.to_path_buf(),
            hash: expected_source.hash.clone(),
            status: expected_source.status.clone(),
            parser_used: expected_source.parser_used.clone(),
            last_ingested_at: expected_source.last_ingested_at.clone(),
        };
        let serialized_locators = expected_evidence
            .iter()
            .zip(relocated_evidence)
            .map(|(expected, relocated)| {
                Ok((
                    expected.id.clone(),
                    serde_json::to_string(&expected.locator)
                        .context("serialize stored relocation locator")?,
                    serde_json::to_string(&relocated.locator)
                        .context("serialize relocated locator")?,
                    GraphNodeId::new(
                        &expected_source.id,
                        GraphNodeKind::EvidenceUnit,
                        &expected.id.0,
                    ),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let old_label = source_file_label(&expected_source.path);
        let new_label = source_file_label(new_path);
        let old_metadata = source_graph_metadata(expected_source, &expected_source.path)?;
        let new_metadata = source_graph_metadata(expected_source, new_path)?;
        let source_node_id = GraphNodeId::new(
            &expected_source.id,
            GraphNodeKind::Source,
            &expected_source.id.0,
        );

        let writer = global_resource_registry().resource_or_insert(
            "sqlite_writer",
            "sqlite_write",
            ResourceLimitConfig {
                capacity: 1,
                queue_capacity: 512,
                queue_timeout: Duration::from_secs(300),
            },
        );
        let _writer_permit = writer
            .acquire_blocking()
            .context("acquire sqlite writer for source relocation")?;
        self.ensure_write_capacity(SqliteWriteOperation::Ingest)?;

        (|| {
            let tx =
                Transaction::new_unchecked(self.connection(), TransactionBehavior::Immediate)?;
            let actual_source = tx
                .query_row(
                    "SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources WHERE id = ?1",
                    params![&expected_source.id.0],
                    row_to_source,
                )
                .optional()?
                .ok_or_else(|| {
                    validation_message(format!(
                        "source disappeared during relocation: {}",
                        bounded_text(&expected_source.id.0)
                    ))
                })?;
            if !sources_equal(&actual_source, expected_source) {
                return Err(validation_message(format!(
                    "source changed during relocation: {}",
                    bounded_text(&expected_source.id.0)
                )));
            }
            if let Some(conflict) = tx
                .query_row(
                    "SELECT id FROM sources WHERE path = ?1 AND id <> ?2 LIMIT 1",
                    params![new_path_text, &expected_source.id.0],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                return Err(validation_message(format!(
                    "relocation path {} belongs to source {}",
                    bounded_path(new_path),
                    bounded_text(&conflict)
                )));
            }
            if let Some(collection) = tx
                .query_row(
                    "SELECT collection_name FROM collection_members WHERE source_id = ?1 ORDER BY collection_name LIMIT 1",
                    params![&expected_source.id.0],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                return Err(validation_message(format!(
                    "source {} belongs to collection {}",
                    bounded_text(&expected_source.id.0),
                    bounded_text(&collection)
                )));
            }
            if let Some(collection) = collection_covering_path(&tx, new_path)? {
                return Err(validation_message(format!(
                    "relocation target is covered by collection root for {}: {}",
                    bounded_text(&collection),
                    bounded_path(new_path)
                )));
            }
            let actual_evidence = direct_text_evidence_tx(&tx, &expected_source.id)?;
            if actual_evidence != expected_evidence {
                return Err(validation_message(format!(
                    "stored evidence changed during relocation for source {}",
                    bounded_text(&expected_source.id.0)
                )));
            }
            if !path_entry_is_missing(&expected_source.path)? {
                return Err(validation_message(format!(
                    "stored source path reappeared during relocation: {}",
                    bounded_path(&expected_source.path)
                )));
            }
            expected_target.validate_content_identity(&expected_source.hash)?;
            expected_target.validate_path_binding(new_path)?;
            #[cfg(test)]
            if let Some(hook) = self
                .source_relocation_before_mutation_hook
                .borrow_mut()
                .take()
            {
                hook();
            }

            require_changed_row(
                tx.execute(
                    "UPDATE sources SET path = ?1
                     WHERE id = ?2 AND path = ?3 AND hash = ?4 AND status = ?5
                       AND parser_used IS ?6 AND last_ingested_at IS ?7",
                    params![
                        new_path_text,
                        &expected_source.id.0,
                        old_path,
                        &expected_source.hash,
                        status_to_str(&expected_source.status),
                        &expected_source.parser_used,
                        &expected_source.last_ingested_at,
                    ],
                )?,
                "source row",
                &expected_source.id.0,
            )?;

            for (evidence_id, old_locator, new_locator, graph_node_id) in &serialized_locators {
                require_changed_row(
                    tx.execute(
                        "UPDATE evidence_units SET locator_json = ?1
                         WHERE id = ?2 AND source_id = ?3 AND kind = 'Text' AND locator_json = ?4",
                        params![new_locator, &evidence_id.0, &expected_source.id.0, old_locator],
                    )?,
                    "evidence locator",
                    &evidence_id.0,
                )?;

                let total_spans: usize = tx.query_row(
                    "SELECT COUNT(*) FROM chunk_evidence_spans WHERE evidence_unit_id = ?1",
                    params![&evidence_id.0],
                    |row| row.get(0),
                )?;
                let matching_spans: usize = tx.query_row(
                    "SELECT COUNT(*) FROM chunk_evidence_spans
                     WHERE evidence_unit_id = ?1 AND locator_json = ?2",
                    params![&evidence_id.0, old_locator],
                    |row| row.get(0),
                )?;
                if total_spans != matching_spans {
                    return Err(validation_message(format!(
                        "span locator mismatch during relocation for evidence {}",
                        bounded_text(&evidence_id.0)
                    )));
                }
                let updated_spans = tx.execute(
                    "UPDATE chunk_evidence_spans SET locator_json = ?1
                     WHERE evidence_unit_id = ?2 AND locator_json = ?3",
                    params![new_locator, &evidence_id.0, old_locator],
                )?;
                if updated_spans != total_spans {
                    return Err(validation_message(format!(
                        "span locator update count changed for evidence {}",
                        bounded_text(&evidence_id.0)
                    )));
                }

                require_changed_row(
                    tx.execute(
                        "UPDATE graph_nodes SET locator_json = ?1
                         WHERE id = ?2 AND source_id = ?3 AND kind = 'EvidenceUnit'
                           AND external_id = ?4 AND locator_json = ?5",
                        params![
                            new_locator,
                            &graph_node_id.0,
                            &expected_source.id.0,
                            &evidence_id.0,
                            old_locator,
                        ],
                    )?,
                    "evidence graph locator",
                    &evidence_id.0,
                )?;
            }

            require_changed_row(
                tx.execute(
                    "UPDATE graph_nodes SET label = ?1, metadata_json = ?2
                     WHERE id = ?3 AND source_id = ?4 AND kind = 'Source'
                       AND external_id = ?4 AND label IS ?5 AND locator_json IS NULL
                       AND ordinal IS NULL AND metadata_json = ?6",
                    params![
                        &new_label,
                        &new_metadata,
                        &source_node_id.0,
                        &expected_source.id.0,
                        &old_label,
                        &old_metadata,
                    ],
                )?,
                "source graph node",
                &source_node_id.0,
            )?;
            if !path_entry_is_missing(&expected_source.path)? {
                return Err(validation_message(format!(
                    "stored source path reappeared before relocation commit: {}",
                    bounded_path(&expected_source.path)
                )));
            }
            expected_target.validate_content_identity(&expected_source.hash)?;
            expected_target.validate_path_binding(new_path)?;
            tx.commit()?;
            Ok(relocated_source)
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }
}

#[cfg(test)]
impl Store {
    pub(crate) fn set_source_relocation_parse_hooks(
        &self,
        before_parse: impl FnOnce() + Send + 'static,
        after_parse: impl FnOnce() + Send + 'static,
    ) {
        let previous_before = self
            .source_relocation_before_parse_hook
            .borrow_mut()
            .replace(Box::new(before_parse));
        let previous_after = self
            .source_relocation_after_parse_hook
            .borrow_mut()
            .replace(Box::new(after_parse));
        assert!(
            previous_before.is_none() && previous_after.is_none(),
            "source relocation parse test hook already set"
        );
    }

    pub(crate) fn set_source_relocation_before_mutation_hook(
        &self,
        hook: impl FnOnce() + Send + 'static,
    ) {
        let previous = self
            .source_relocation_before_mutation_hook
            .borrow_mut()
            .replace(Box::new(hook));
        assert!(
            previous.is_none(),
            "source relocation test hook already set"
        );
    }
}

fn relocation_error(kind: SourceRelocationErrorKind, source: anyhow::Error) -> anyhow::Error {
    SourceRelocationError { kind, source }.into()
}

fn validation_error(source: anyhow::Error) -> anyhow::Error {
    relocation_error(SourceRelocationErrorKind::Validation, source)
}

fn validation_message(message: impl Into<String>) -> anyhow::Error {
    validation_error(anyhow::anyhow!(message.into()))
}

fn path_entry_is_missing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => Err(validation_error(
            anyhow::Error::new(error)
                .context(format!("check source path entry: {}", bounded_path(path))),
        )),
        Err(error) => {
            Err(error).with_context(|| format!("check source path entry: {}", bounded_path(path)))
        }
    }
}

/// Remap path-keyed IDs while retaining strict self-contained canonical JSONL IDs.
pub(crate) fn remap_parser_evidence_identity(
    evidence: Vec<EvidenceUnit>,
    parser_source_id: &SourceId,
    catalog_source_id: &SourceId,
) -> Result<Vec<EvidenceUnit>> {
    let mut remapped_ids = HashMap::with_capacity(evidence.len());
    let mut output_ids = HashSet::with_capacity(evidence.len());
    for unit in &evidence {
        if unit.source_id != *parser_source_id {
            bail!(
                "parser evidence {} has source {}, expected {}",
                bounded_text(&unit.id.0),
                bounded_text(&unit.source_id.0),
                bounded_text(&parser_source_id.0)
            );
        }
        let remapped = match unit.id.0.strip_prefix(&parser_source_id.0) {
            Some(suffix) if suffix.starts_with(':') => {
                EvidenceId(format!("{}{suffix}", catalog_source_id.0))
            }
            _ if parser::canonical_jsonl::is_generated_evidence_id(&unit.id) => unit.id.clone(),
            _ => bail!(
                "parser evidence id is neither source-prefixed by {} nor a valid self-contained canonical JSONL id: {}",
                bounded_text(&parser_source_id.0),
                bounded_text(&unit.id.0)
            ),
        };
        if remapped_ids
            .insert(unit.id.clone(), remapped.clone())
            .is_some()
        {
            bail!("duplicate parser evidence id: {}", bounded_text(&unit.id.0));
        }
        if !output_ids.insert(remapped.clone()) {
            bail!(
                "duplicate remapped evidence id: {}",
                bounded_text(&remapped.0)
            );
        }
    }

    evidence
        .into_iter()
        .map(|mut unit| {
            let old_id = unit.id.clone();
            unit.id = remapped_ids
                .get(&old_id)
                .cloned()
                .context("complete parser evidence remap is missing an id")?;
            unit.source_id = catalog_source_id.clone();
            unit.derived_from = unit
                .derived_from
                .as_ref()
                .map(|derived_from| {
                    remapped_ids.get(derived_from).cloned().with_context(|| {
                        format!(
                            "parser evidence {} has dangling derived_from {}",
                            bounded_text(&old_id.0),
                            bounded_text(&derived_from.0)
                        )
                    })
                })
                .transpose()?;
            Ok(unit)
        })
        .collect()
}

fn rewrite_relocation_locator_paths(
    evidence: &mut [EvidenceUnit],
    parser_path: &Path,
    canonical_path: &Path,
) -> Result<()> {
    let parser_path = parser_path
        .to_str()
        .context("held relocation parser path is not UTF-8")?;
    let canonical_path = canonical_path
        .to_str()
        .context("canonical relocation target is not UTF-8")?;
    for unit in evidence {
        let path = match &mut unit.locator {
            SourceLocator::Document { path_or_url, .. } => Some(path_or_url),
            SourceLocator::Markdown { path, .. } => Some(path),
            _ => None,
        };
        if let Some(path) = path {
            if path.as_str() != parser_path {
                bail!(
                    "parser evidence {} locator does not identify the held snapshot",
                    bounded_text(&unit.id.0)
                );
            }
            canonical_path.clone_into(path);
        }
    }
    Ok(())
}

pub(super) fn ensure_unique_source_paths(conn: &Connection) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT path, COUNT(*) FROM sources
         GROUP BY path HAVING COUNT(*) > 1 ORDER BY path LIMIT 5",
    )?;
    let duplicates = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !duplicates.is_empty() {
        let context = duplicates
            .iter()
            .map(|(path, count)| format!("{} ({count} rows)", bounded_text(path)))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "cannot enforce unique source paths; legacy duplicate source locations require manual resolution: {context}"
        );
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS sources_path_unique_idx ON sources(path);",
    )
    .context(
        "enforce unique source paths; legacy duplicate source locations require manual resolution",
    )
}

pub(super) fn row_to_source(row: &Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        id: SourceId(row.get(0)?),
        path: PathBuf::from(row.get::<_, String>(1)?),
        hash: row.get(2)?,
        status: str_to_status(&row.get::<_, String>(3)?),
        parser_used: row.get(4)?,
        last_ingested_at: row.get(5)?,
    })
}

fn validate_relocation_evidence(stored: &[EvidenceUnit], parsed: &[EvidenceUnit]) -> Result<()> {
    if stored.len() != parsed.len() {
        bail!(
            "relocation parser evidence count changed from {} to {}",
            stored.len(),
            parsed.len()
        );
    }
    for (stored, parsed) in stored.iter().zip(parsed) {
        if stored.id != parsed.id
            || stored.source_id != parsed.source_id
            || stored.kind != parsed.kind
            || stored.derived_from != parsed.derived_from
            || stored.text != parsed.text
            || stored.text_hash != parsed.text_hash
            || stored.heading_path != parsed.heading_path
            || stored.position != parsed.position
            || !relocation_locator_matches(&stored.locator, &parsed.locator)
        {
            bail!(
                "relocation parser evidence changed at {}",
                bounded_text(&stored.id.0)
            );
        }
    }
    Ok(())
}

fn relocation_locator_matches(stored: &SourceLocator, parsed: &SourceLocator) -> bool {
    let mut expected = stored.clone();
    match (&mut expected, parsed) {
        (
            SourceLocator::Document { path_or_url, .. },
            SourceLocator::Document {
                path_or_url: parsed_path,
                ..
            },
        ) => parsed_path.clone_into(path_or_url),
        (
            SourceLocator::Markdown { path, .. },
            SourceLocator::Markdown {
                path: parsed_path, ..
            },
        ) => parsed_path.clone_into(path),
        _ => {}
    }
    expected == *parsed
}

fn direct_text_evidence_tx(
    tx: &Transaction<'_>,
    source_id: &SourceId,
) -> Result<Vec<EvidenceUnit>> {
    let mut statement = tx.prepare(
        "SELECT id, source_id, kind, locator_json, text, text_hash, heading_path_json,
                position, derived_from_evidence_id
         FROM evidence_units WHERE source_id = ?1 AND kind = 'Text' ORDER BY position",
    )?;
    let rows = statement.query_map(params![&source_id.0], row_to_evidence_unit)?;
    rows.map(|row| row.map_err(Into::into)).collect()
}

fn evidence_matches_except_locator(expected: &EvidenceUnit, relocated: &EvidenceUnit) -> bool {
    expected.id == relocated.id
        && expected.source_id == relocated.source_id
        && expected.kind == relocated.kind
        && expected.derived_from == relocated.derived_from
        && expected.text == relocated.text
        && expected.text_hash == relocated.text_hash
        && expected.heading_path == relocated.heading_path
        && expected.position == relocated.position
}

fn sources_equal(left: &Source, right: &Source) -> bool {
    left.id == right.id
        && left.path == right.path
        && left.hash == right.hash
        && left.status == right.status
        && left.parser_used == right.parser_used
        && left.last_ingested_at == right.last_ingested_at
}

fn source_file_label(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
}

fn source_graph_metadata(source: &Source, path: &Path) -> Result<String> {
    serde_json::to_string(&serde_json::json!({
        "path": path.to_string_lossy(),
        "hash": &source.hash,
        "parser_used": &source.parser_used,
    }))
    .context("serialize source graph relocation metadata")
}

fn require_changed_row(changed: usize, row_kind: &str, identity: &str) -> Result<()> {
    if changed != 1 {
        return Err(validation_message(format!(
            "relocation guarded update matched {changed} {row_kind} rows for {}",
            bounded_text(identity)
        )));
    }
    Ok(())
}

fn bounded_text(value: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = value.chars();
    let bounded = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn bounded_path(path: &Path) -> String {
    bounded_text(&path.to_string_lossy())
}
