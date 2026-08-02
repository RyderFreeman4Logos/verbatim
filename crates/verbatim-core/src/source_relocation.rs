//! Validation and atomic persistence for explicit, identity-preserving source relocation.
//!
//! Parser output is path-keyed, while the catalog identity must remain stable across a
//! relocation. This module owns the one fail-closed remap seam and the one SQLite
//! transaction that updates location-bearing catalog fields.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::{
    map_storage_error, row_to_evidence_unit, status_to_str, str_to_status, SqliteWriteOperation,
    Store,
};
use crate::ingest::{source_path_is_missing, IngestPipeline};
use crate::ingest_security::InputSnapshotIdentity;
use crate::parser;
use crate::resource::{global_resource_registry, ResourceLimitConfig};
use crate::traits::EmbeddingClient;
use crate::types::{
    EvidenceId, EvidenceKind, EvidenceUnit, GraphNodeId, GraphNodeKind, Source, SourceId,
    SourceLocator, SourceStatus,
};

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    /// Record an external, content-identical file relocation without changing catalog identity.
    pub fn relocate_source(&mut self, source_id: &SourceId, new_path: &Path) -> Result<Source> {
        let operation = (|| {
            let source = self
                .store()
                .get_source(source_id)?
                .with_context(|| format!("source not found: {}", bounded_text(&source_id.0)))?;
            if source.status != SourceStatus::Indexed {
                bail!("source is not indexed");
            }
            let parser_used = source
                .parser_used
                .as_deref()
                .context("source has no previous parser")?;
            if !source_path_is_missing(&source.path)? {
                bail!(
                    "stored source path still exists: {}",
                    bounded_path(&source.path)
                );
            }

            let canonical_path = fs::canonicalize(new_path).with_context(|| {
                format!("resolve relocation target: {}", bounded_path(new_path))
            })?;
            canonical_path.to_str().with_context(|| {
                format!(
                    "relocation target is not UTF-8: {}",
                    bounded_path(&canonical_path)
                )
            })?;
            if let Some(conflict) = self.store().get_source_by_path(&canonical_path)? {
                if conflict.id != *source_id {
                    bail!(
                        "relocation target belongs to source {}: {}",
                        bounded_text(&conflict.id.0),
                        bounded_path(&canonical_path)
                    );
                }
            }

            let parser = parser::parser_for_extension(&canonical_path)?;
            if parser.name() != parser_used {
                bail!(
                    "relocation parser changed from {} to {}",
                    bounded_text(parser_used),
                    bounded_text(parser.name())
                );
            }
            let before = InputSnapshotIdentity::from_path(&canonical_path)?;
            if before.content_sha256 != source.hash {
                bail!("relocation target content hash differs from stored source hash");
            }
            let parsed = remap_parser_evidence_identity(
                parser.parse(&canonical_path)?,
                &SourceId::from_path(&canonical_path),
                source_id,
            )?;
            let after = InputSnapshotIdentity::from_path(&canonical_path)?;
            if before != after {
                bail!("relocation target changed while it was parsed");
            }
            if after.content_sha256 != source.hash {
                bail!("relocation target content hash changed from stored source hash");
            }

            let stored = self
                .store()
                .list_evidence_by_source(source_id)?
                .into_iter()
                .filter(|unit| unit.kind == EvidenceKind::Text)
                .collect::<Vec<_>>();
            validate_relocation_evidence(&stored, &parsed)?;
            self.store()
                .relocate_source(&source, &canonical_path, &stored, &parsed)
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

    pub(crate) fn relocate_source(
        &self,
        expected_source: &Source,
        new_path: &Path,
        expected_evidence: &[EvidenceUnit],
        relocated_evidence: &[EvidenceUnit],
    ) -> Result<Source> {
        if expected_evidence.len() != relocated_evidence.len()
            || expected_evidence
                .iter()
                .zip(relocated_evidence)
                .any(|(expected, relocated)| !evidence_matches_except_locator(expected, relocated))
        {
            bail!(
                "relocation evidence changed for source {}",
                bounded_text(&expected_source.id.0)
            );
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
                .with_context(|| {
                    format!(
                        "source disappeared during relocation: {}",
                        bounded_text(&expected_source.id.0)
                    )
                })?;
            if !sources_equal(&actual_source, expected_source) {
                bail!(
                    "source changed during relocation: {}",
                    bounded_text(&expected_source.id.0)
                );
            }
            if let Some(conflict) = tx
                .query_row(
                    "SELECT id FROM sources WHERE path = ?1 AND id <> ?2 LIMIT 1",
                    params![new_path_text, &expected_source.id.0],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                bail!(
                    "relocation path {} belongs to source {}",
                    bounded_path(new_path),
                    bounded_text(&conflict)
                );
            }
            if let Some(collection) = tx
                .query_row(
                    "SELECT collection_name FROM collection_members WHERE source_id = ?1 ORDER BY collection_name LIMIT 1",
                    params![&expected_source.id.0],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                bail!(
                    "source {} belongs to collection {}",
                    bounded_text(&expected_source.id.0),
                    bounded_text(&collection)
                );
            }
            let actual_evidence = direct_text_evidence_tx(&tx, &expected_source.id)?;
            if actual_evidence != expected_evidence {
                bail!(
                    "stored evidence changed during relocation for source {}",
                    bounded_text(&expected_source.id.0)
                );
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
                    bail!(
                        "span locator mismatch during relocation for evidence {}",
                        bounded_text(&evidence_id.0)
                    );
                }
                let updated_spans = tx.execute(
                    "UPDATE chunk_evidence_spans SET locator_json = ?1
                     WHERE evidence_unit_id = ?2 AND locator_json = ?3",
                    params![new_locator, &evidence_id.0, old_locator],
                )?;
                if updated_spans != total_spans {
                    bail!(
                        "span locator update count changed for evidence {}",
                        bounded_text(&evidence_id.0)
                    );
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
            tx.commit()?;
            Ok(relocated_source)
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }
}

/// Remap a complete parser batch from temporary path identity to catalog identity.
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
        let suffix = unit
            .id
            .0
            .strip_prefix(&parser_source_id.0)
            .filter(|suffix| suffix.starts_with(':'))
            .with_context(|| {
                format!(
                    "parser evidence id is empty or lacks source prefix {}: {}",
                    bounded_text(&parser_source_id.0),
                    bounded_text(&unit.id.0)
                )
            })?;
        let remapped = EvidenceId(format!("{}{suffix}", catalog_source_id.0));
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
        bail!(
            "relocation guarded update matched {changed} {row_kind} rows for {}",
            bounded_text(identity)
        );
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
