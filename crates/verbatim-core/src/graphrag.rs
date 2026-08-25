//! Bounded GraphRAG global and local search over the stored evidence graph.
//!
//! The module is deliberately deterministic. It canonicalizes generated
//! entities, detects stable connected-component communities, builds community
//! reports only from graph items that retain source-span evidence, and keeps
//! generated report prose out of ordinary evidence.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::GraphGlobalSearchConfig;
use crate::store::Store;
use crate::types::report_artifact::ReportArtifactId;
use crate::types::{
    hex_sha256, ChunkId, EvidenceId, EvidenceUnit, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId,
    GraphNodeKind, RetrievalResult, SourceId,
};

mod resolve_report_artifact;
pub use resolve_report_artifact::ReportArtifactManifest;

const HARD_MAX_COMMUNITIES: usize = 256;
const HARD_MAX_REPORT_CLAIMS: usize = 24;
const HARD_MAX_REPORT_CHARS: usize = 8_000;
const HARD_MAX_EVIDENCE_PER_REPORT: usize = 24;
const HARD_MAX_SEARCH_RESULTS: usize = 12;

/// Canonical deterministic grouping for generated entity nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalEntity {
    pub id: String,
    pub key: String,
    pub entity_type: String,
    pub label: String,
    pub node_ids: Vec<GraphNodeId>,
    pub source_spans: Vec<GraphSourceSpan>,
}

/// Source span retained from generated graph metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphSourceSpan {
    pub raw: String,
    pub chunk_id: ChunkId,
    pub line_start: u32,
    pub line_end: u32,
}

/// Stable connected community over generated graph nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphCommunity {
    pub id: String,
    pub node_ids: Vec<GraphNodeId>,
    pub edge_ids: Vec<GraphEdgeId>,
}

/// Evidence retained by a community report claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityReportEvidence {
    pub source_span: GraphSourceSpan,
    pub evidence: EvidenceUnit,
}

/// One evidence-backed claim included in a community report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityReportClaim {
    pub text: String,
    pub node_id: Option<GraphNodeId>,
    pub edge_id: Option<GraphEdgeId>,
    pub evidence_ids: Vec<EvidenceId>,
    pub source_spans: Vec<GraphSourceSpan>,
}

/// Deterministic report used by GraphRAG global search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommunityReport {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub claims: Vec<CommunityReportClaim>,
    pub evidence: Vec<CommunityReportEvidence>,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub generation: String,
}
impl CommunityReport {
    /// Recompute the SHA-256 digest of the report payload.
    pub fn recompute_content_hash(&self) -> Result<String> {
        let mut payload = serde_json::to_value(self)?;
        payload
            .as_object_mut()
            .expect("CommunityReport serializes as an object")
            .remove("content_hash");
        Ok(hex_sha256(&serde_json::to_vec(&payload)?))
    }
}
/// Ranked community report hit for a broad corpus-level query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalSearchHit {
    pub rank: usize,
    pub score: f32,
    /// Canonical identity for the report artifact represented by this hit.
    pub report_artifact_id: ReportArtifactId,
    pub report: CommunityReport,
}

/// Local chunk hit enriched with nearby generated graph entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGraphChunkHit {
    pub rank: usize,
    pub chunk_id: ChunkId,
    pub source_id: SourceId,
    pub evidence_ids: Vec<EvidenceId>,
    pub entity_node_ids: Vec<GraphNodeId>,
}

/// Local graph+chunk search result preserving raw chunk citations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalGraphChunkSearch {
    pub hits: Vec<LocalGraphChunkHit>,
}

/// Store-backed GraphRAG service.
pub struct GraphRagService<'a> {
    store: &'a Store,
    config: &'a GraphGlobalSearchConfig,
}

impl<'a> GraphRagService<'a> {
    pub fn new(store: &'a Store, config: &'a GraphGlobalSearchConfig) -> Self {
        Self { store, config }
    }

    pub fn community_reports(
        &self,
        source_filter: Option<&SourceId>,
    ) -> Result<Vec<CommunityReport>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }
        let nodes = match source_filter {
            Some(source_id) => self.store.list_graph_nodes_by_source(source_id)?,
            None => self.store.list_graph_nodes()?,
        };
        let edges = match source_filter {
            Some(source_id) => self.store.list_graph_edges_by_source(source_id)?,
            None => self.store.list_graph_edges()?,
        };
        let communities = detect_communities(&nodes, &edges);
        build_community_reports(self.store, &nodes, &edges, &communities, self.config)
    }

    pub fn global_search(
        &self,
        query: &str,
        source_filter: Option<&SourceId>,
    ) -> Result<Vec<GlobalSearchHit>> {
        let reports = self.community_reports(source_filter)?;
        Ok(search_community_reports(query, &reports, self.config))
    }

    /// Return only Store-backed evidence selected by structured global search.
    pub fn global_search_backing_results(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<RetrievalResult>> {
        let max_results = bounded_nonzero(
            self.config.max_search_results,
            HARD_MAX_SEARCH_RESULTS,
            GraphGlobalSearchConfig::default().max_search_results,
        );
        let mut hits = Vec::new();
        match source_filter {
            None => hits = self.global_search(query, None)?,
            Some(source_ids) => {
                let mut source_ids = source_ids.iter().collect::<Vec<_>>();
                source_ids.sort();
                for source_id in source_ids {
                    hits.extend(self.global_search(query, Some(source_id))?);
                    hits.sort_by(global_hit_order);
                    hits.truncate(max_results);
                }
            }
        }
        backing_results_from_hits(self.store, hits, max_results)
    }

    pub fn local_search(&self, results: &[RetrievalResult]) -> Result<LocalGraphChunkSearch> {
        local_graph_chunk_search(self.store, results)
    }
}

/// Canonicalize generated entity nodes with stable grouping and ordering.
pub fn canonicalize_entities(nodes: &[GraphNode]) -> Vec<CanonicalEntity> {
    let mut groups: BTreeMap<String, CanonicalEntityBuilder> = BTreeMap::new();
    for node in nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::GeneratedEntity)
    {
        let label = node_label(node);
        if label.is_empty() {
            continue;
        }
        let entity_type = metadata_string(node.metadata.as_ref(), "entity_type")
            .unwrap_or_else(|| "other".to_string());
        let key = canonical_entity_key(&entity_type, &label);
        let builder = groups.entry(key.clone()).or_insert_with(|| {
            CanonicalEntityBuilder::new(key.clone(), entity_type.clone(), label.clone())
        });
        builder.labels.insert(label);
        builder.node_ids.insert(node.id.clone());
        for span in metadata_source_spans(node.metadata.as_ref()) {
            builder.source_spans.insert(span);
        }
    }

    groups
        .into_values()
        .map(CanonicalEntityBuilder::finish)
        .collect()
}

/// Detect stable connected components over generated graph nodes.
pub fn detect_communities(nodes: &[GraphNode], edges: &[GraphEdge]) -> Vec<GraphCommunity> {
    let generated_nodes = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                GraphNodeKind::GeneratedEntity | GraphNodeKind::GeneratedClaim
            )
        })
        .collect::<Vec<_>>();
    if generated_nodes.is_empty() {
        return Vec::new();
    }

    let relevant = generated_nodes
        .iter()
        .map(|node| node.id.0.clone())
        .collect::<BTreeSet<_>>();
    let mut union = UnionFind::new(relevant.iter().cloned());

    for entity in canonicalize_entities(nodes) {
        let mut ids = entity.node_ids.iter().map(|id| id.0.as_str());
        let Some(first) = ids.next() else {
            continue;
        };
        for next in ids {
            union.union(first, next);
        }
    }

    for edge in edges {
        if relevant.contains(&edge.from_node_id.0) && relevant.contains(&edge.to_node_id.0) {
            union.union(&edge.from_node_id.0, &edge.to_node_id.0);
        }
    }

    let mut node_ids_by_label: HashMap<String, Vec<String>> = HashMap::new();
    for node in &generated_nodes {
        let labels = match node.kind {
            GraphNodeKind::GeneratedEntity => vec![node_label(node)],
            GraphNodeKind::GeneratedClaim => ["subject", "object"]
                .into_iter()
                .filter_map(|field| metadata_string(node.metadata.as_ref(), field))
                .collect(),
            _ => Vec::new(),
        };
        for label in labels {
            let key = lookup_key(&label);
            if !key.is_empty() {
                node_ids_by_label
                    .entry(key)
                    .or_default()
                    .push(node.id.0.clone());
            }
        }
    }

    for node in generated_nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::GeneratedClaim)
    {
        for field in ["subject", "object"] {
            let Some(value) = metadata_string(node.metadata.as_ref(), field) else {
                continue;
            };
            let key = lookup_key(&value);
            if let Some(node_ids) = node_ids_by_label.get(&key) {
                for node_id in node_ids {
                    union.union(&node.id.0, node_id);
                }
            }
        }
    }

    let mut node_ids_by_root: BTreeMap<String, BTreeSet<GraphNodeId>> = BTreeMap::new();
    for node in generated_nodes {
        let root = union.find(&node.id.0);
        node_ids_by_root
            .entry(root)
            .or_default()
            .insert(node.id.clone());
    }

    let mut communities = Vec::new();
    for node_ids in node_ids_by_root.into_values() {
        let node_key_set = node_ids
            .iter()
            .map(|node_id| node_id.0.clone())
            .collect::<BTreeSet<_>>();
        let edge_ids = edges
            .iter()
            .filter(|edge| {
                node_key_set.contains(&edge.from_node_id.0)
                    && node_key_set.contains(&edge.to_node_id.0)
            })
            .map(|edge| edge.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let node_ids = node_ids.into_iter().collect::<Vec<_>>();
        let id = stable_community_id(&node_ids);
        communities.push(GraphCommunity {
            id,
            node_ids,
            edge_ids,
        });
    }

    communities.sort_by(|left, right| left.id.cmp(&right.id));
    communities
}

/// Build deterministic community reports and drop claims lacking evidence.
pub fn build_community_reports(
    store: &Store,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    communities: &[GraphCommunity],
    config: &GraphGlobalSearchConfig,
) -> Result<Vec<CommunityReport>> {
    if !config.enabled {
        return Ok(Vec::new());
    }

    let max_communities = bounded_nonzero(
        config.max_communities,
        HARD_MAX_COMMUNITIES,
        GraphGlobalSearchConfig::default().max_communities,
    );
    let max_claims = bounded_nonzero(
        config.max_report_claims,
        HARD_MAX_REPORT_CLAIMS,
        GraphGlobalSearchConfig::default().max_report_claims,
    );
    let max_chars = bounded_nonzero(
        config.max_report_chars,
        HARD_MAX_REPORT_CHARS,
        GraphGlobalSearchConfig::default().max_report_chars,
    );
    let max_evidence = bounded_nonzero(
        config.max_evidence_per_report,
        HARD_MAX_EVIDENCE_PER_REPORT,
        GraphGlobalSearchConfig::default().max_evidence_per_report,
    );

    let nodes_by_id = nodes
        .iter()
        .map(|node| (node.id.0.clone(), node))
        .collect::<HashMap<_, _>>();
    let edges_by_id = edges
        .iter()
        .map(|edge| (edge.id.0.clone(), edge))
        .collect::<HashMap<_, _>>();

    let mut reports = Vec::new();
    for community in communities.iter().take(max_communities) {
        let mut claims = Vec::new();

        for node_id in &community.node_ids {
            let Some(node) = nodes_by_id.get(&node_id.0) else {
                continue;
            };
            if let Some(claim) = claim_from_node(store, node)? {
                claims.push(claim);
            }
        }
        for edge_id in &community.edge_ids {
            let Some(edge) = edges_by_id.get(&edge_id.0) else {
                continue;
            };
            if let Some(claim) = claim_from_edge(store, edge)? {
                claims.push(claim);
            }
        }
        claims.sort_by(|left, right| {
            left.text
                .cmp(&right.text)
                .then_with(|| {
                    optional_node_key(&left.node_id).cmp(&optional_node_key(&right.node_id))
                })
                .then_with(|| {
                    optional_edge_key(&left.edge_id).cmp(&optional_edge_key(&right.edge_id))
                })
        });
        claims.truncate(max_claims);
        if claims.is_empty() {
            continue;
        }
        let evidence = collect_claim_evidence(store, &claims, max_evidence)?;
        let backing_evidence_ids = evidence
            .iter()
            .map(|backing| &backing.evidence.id)
            .collect::<BTreeSet<_>>();
        claims.retain(|claim| {
            !claim.evidence_ids.is_empty()
                && claim
                    .evidence_ids
                    .iter()
                    .all(|evidence_id| backing_evidence_ids.contains(evidence_id))
        });
        if claims.is_empty() {
            continue;
        }

        let title = community_title(community, &nodes_by_id);
        let summary = build_report_summary(&title, &claims, max_chars);
        let mut report = CommunityReport {
            id: community.id.clone(),
            title,
            summary,
            claims,
            evidence,
            content_hash: String::new(),
            generation: hex_sha256(serde_json::to_string(community)?.as_bytes()),
        };
        report.content_hash = report.recompute_content_hash()?;
        reports.push(report);
    }

    reports.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(reports)
}

/// Search community reports with deterministic lexical ranking.
pub fn search_community_reports(
    query: &str,
    reports: &[CommunityReport],
    config: &GraphGlobalSearchConfig,
) -> Vec<GlobalSearchHit> {
    if !config.enabled || reports.is_empty() {
        return Vec::new();
    }

    let max_results = bounded_nonzero(
        config.max_search_results,
        HARD_MAX_SEARCH_RESULTS,
        GraphGlobalSearchConfig::default().max_search_results,
    );
    let mut scored = reports
        .iter()
        .cloned()
        .map(|report| {
            let score = report_score(query, &report);
            (score, report)
        })
        .collect::<Vec<_>>();

    if !query.trim().is_empty() {
        scored.retain(|(score, _)| *score > 0.0);
    }

    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left.id.cmp(&right.id))
    });

    scored
        .into_iter()
        .filter_map(|(score, report)| {
            let report_artifact_id = ReportArtifactId::new(&report.id).ok()?;
            Some((score, report_artifact_id, report))
        })
        .take(max_results)
        .enumerate()
        .map(
            |(rank, (score, report_artifact_id, report))| GlobalSearchHit {
                rank: rank + 1,
                score,
                report_artifact_id,
                report,
            },
        )
        .collect()
}

fn backing_results_from_hits(
    store: &Store,
    mut hits: Vec<GlobalSearchHit>,
    max_results: usize,
) -> Result<Vec<RetrievalResult>> {
    hits.sort_by(global_hit_order);

    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for hit in hits {
        for backing in hit.report.evidence {
            if results.len() >= max_results {
                return Ok(results);
            }
            if seen.contains(&backing.evidence.id) {
                continue;
            }
            let Some(chunk) = store.get_chunk(&backing.source_span.chunk_id)? else {
                continue;
            };
            let Some(evidence) = store.get_evidence(&backing.evidence.id)? else {
                continue;
            };
            if chunk.source_id != evidence.source_id
                || !chunk.evidence_unit_ids.contains(&evidence.id)
            {
                continue;
            }
            seen.insert(evidence.id.clone());
            let result_rank = results.len() + 1;
            results.push(RetrievalResult {
                chunk_id: chunk.id.clone(),
                score: hit.score,
                provenance: crate::retrieve::graph_report_provenance(
                    result_rank,
                    hit.report_artifact_id.clone(),
                ),
                chunk,
                evidence_units: vec![evidence],
            });
        }
    }
    Ok(results)
}

fn global_hit_order(left: &GlobalSearchHit, right: &GlobalSearchHit) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.rank.cmp(&right.rank))
        .then_with(|| left.report.id.cmp(&right.report.id))
}

/// Enrich local retrieval results with graph entity ids while preserving raw evidence ids.
pub fn local_graph_chunk_search(
    store: &Store,
    results: &[RetrievalResult],
) -> Result<LocalGraphChunkSearch> {
    let mut nodes_by_source: HashMap<String, Vec<GraphNode>> = HashMap::new();
    let mut hits = Vec::new();

    for (idx, result) in results.iter().enumerate() {
        let source_id = &result.chunk.source_id;
        if !nodes_by_source.contains_key(&source_id.0) {
            nodes_by_source.insert(
                source_id.0.clone(),
                store.list_graph_nodes_by_source(source_id)?,
            );
        }
        let nodes = nodes_by_source
            .get(&source_id.0)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let entity_node_ids = nodes_for_chunk(&result.chunk_id, nodes);
        let evidence_ids = result
            .evidence_units
            .iter()
            .map(|evidence| evidence.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        hits.push(LocalGraphChunkHit {
            rank: idx + 1,
            chunk_id: result.chunk_id.clone(),
            source_id: source_id.clone(),
            evidence_ids,
            entity_node_ids,
        });
    }

    Ok(LocalGraphChunkSearch { hits })
}

fn claim_from_node(store: &Store, node: &GraphNode) -> Result<Option<CommunityReportClaim>> {
    let spans = metadata_source_spans(node.metadata.as_ref());
    if spans.is_empty() {
        return Ok(None);
    }
    let text = match node.kind {
        GraphNodeKind::GeneratedClaim => metadata_string(node.metadata.as_ref(), "claim")
            .or_else(|| node.label.clone())
            .unwrap_or_default(),
        GraphNodeKind::GeneratedEntity => {
            let description =
                metadata_string(node.metadata.as_ref(), "description").unwrap_or_default();
            if description.is_empty() {
                return Ok(None);
            }
            format!("{}: {description}", node_label(node))
        }
        _ => return Ok(None),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let evidence_ids = evidence_ids_for_spans(store, &spans)?;
    if evidence_ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(CommunityReportClaim {
        text: bounded_chars(text.trim(), 1_000),
        node_id: Some(node.id.clone()),
        edge_id: None,
        evidence_ids,
        source_spans: spans,
    }))
}

fn claim_from_edge(store: &Store, edge: &GraphEdge) -> Result<Option<CommunityReportClaim>> {
    let spans = metadata_source_spans(edge.metadata.as_ref());
    if spans.is_empty() {
        return Ok(None);
    }
    let source = metadata_string(edge.metadata.as_ref(), "source")
        .unwrap_or_else(|| edge.from_node_id.0.clone());
    let target = metadata_string(edge.metadata.as_ref(), "target")
        .unwrap_or_else(|| edge.to_node_id.0.clone());
    let relation = metadata_string(edge.metadata.as_ref(), "relationship_type")
        .unwrap_or_else(|| edge.edge_type.as_str().to_string());
    let description = metadata_string(edge.metadata.as_ref(), "description").unwrap_or_default();
    let text = if description.is_empty() {
        format!("{source} {relation} {target}")
    } else {
        format!("{source} {relation} {target}: {description}")
    };
    let evidence_ids = evidence_ids_for_spans(store, &spans)?;
    if evidence_ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(CommunityReportClaim {
        text: bounded_chars(text.trim(), 1_000),
        node_id: None,
        edge_id: Some(edge.id.clone()),
        evidence_ids,
        source_spans: spans,
    }))
}

fn collect_claim_evidence(
    store: &Store,
    claims: &[CommunityReportClaim],
    max_evidence: usize,
) -> Result<Vec<CommunityReportEvidence>> {
    let mut evidence_by_id: BTreeMap<String, CommunityReportEvidence> = BTreeMap::new();
    for claim in claims {
        for span in &claim.source_spans {
            let Some(chunk) = store.get_chunk(&span.chunk_id)? else {
                continue;
            };
            for evidence_id in &chunk.evidence_unit_ids {
                if evidence_by_id.len() >= max_evidence
                    && !evidence_by_id.contains_key(&evidence_id.0)
                {
                    continue;
                }
                let Some(evidence) = store.get_evidence(evidence_id)? else {
                    continue;
                };
                evidence_by_id
                    .entry(evidence.id.0.clone())
                    .or_insert_with(|| CommunityReportEvidence {
                        source_span: span.clone(),
                        evidence,
                    });
            }
        }
    }
    Ok(evidence_by_id.into_values().take(max_evidence).collect())
}

fn evidence_ids_for_spans(store: &Store, spans: &[GraphSourceSpan]) -> Result<Vec<EvidenceId>> {
    let mut ids = BTreeSet::new();
    for span in spans {
        let Some(chunk) = store.get_chunk(&span.chunk_id)? else {
            continue;
        };
        for evidence_id in chunk.evidence_unit_ids {
            if store.get_evidence(&evidence_id)?.is_some() {
                ids.insert(evidence_id);
            }
        }
    }
    Ok(ids.into_iter().collect())
}

fn report_score(query: &str, report: &CommunityReport) -> f32 {
    if query.trim().is_empty() {
        return report.claims.len() as f32 * 0.001;
    }

    let query_terms = token_set(query);
    let searchable = format!(
        "{}\n{}\n{}",
        report.title,
        report.summary,
        report
            .claims
            .iter()
            .map(|claim| claim.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (query_terms, text_terms) = if query_terms.is_empty() {
        (inclusive_token_set(query), inclusive_token_set(&searchable))
    } else {
        (query_terms, token_set(&searchable))
    };
    if query_terms.is_empty() {
        return 0.0;
    }
    let overlap = query_terms
        .iter()
        .filter(|term| text_terms.contains(*term))
        .count() as f32;
    let query_lower = query.to_lowercase();
    let query_lower = query_lower.trim();
    let exact_bonus = (!query_lower.is_empty()
        && (overlap > 0.0 || !query_lower.is_ascii())
        && searchable.to_lowercase().contains(query_lower)) as u8 as f32;
    overlap + exact_bonus
}

fn token_set(text: &str) -> BTreeSet<String> {
    inclusive_token_set(text)
        .into_iter()
        .filter(|token| token.chars().count() > 2 || !token.is_ascii())
        .collect()
}

fn inclusive_token_set(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn nodes_for_chunk(chunk_id: &ChunkId, nodes: &[GraphNode]) -> Vec<GraphNodeId> {
    nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                GraphNodeKind::GeneratedEntity | GraphNodeKind::GeneratedClaim
            ) && metadata_source_spans(node.metadata.as_ref())
                .iter()
                .any(|span| span.chunk_id == *chunk_id)
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn community_title(
    community: &GraphCommunity,
    nodes_by_id: &HashMap<String, &GraphNode>,
) -> String {
    let mut labels = community
        .node_ids
        .iter()
        .filter_map(|node_id| nodes_by_id.get(&node_id.0))
        .filter(|node| node.kind == GraphNodeKind::GeneratedEntity)
        .map(|node| node_label(node))
        .filter(|label| !label.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(3)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        labels.push("Graph community".to_string());
    }
    labels.join(" / ")
}

fn build_report_summary(title: &str, claims: &[CommunityReportClaim], max_chars: usize) -> String {
    let mut summary = format!("{title}. ");
    for claim in claims {
        if !summary.ends_with(' ') {
            summary.push(' ');
        }
        summary.push_str(&claim.text);
        if !summary.ends_with('.') {
            summary.push('.');
        }
        if summary.chars().count() >= max_chars {
            break;
        }
    }
    bounded_chars(summary.trim(), max_chars)
}

fn metadata_source_spans(metadata: Option<&serde_json::Value>) -> Vec<GraphSourceSpan> {
    metadata
        .and_then(|value| value.get("source_spans"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(parse_source_span)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}

fn parse_source_span(raw: &str) -> Option<GraphSourceSpan> {
    let (chunk_id, range) = raw.rsplit_once(':')?;
    let (line_start, line_end) = range.split_once('-')?;
    let line_start = line_start.parse::<u32>().ok()?;
    let line_end = line_end.parse::<u32>().ok()?;
    if line_start == 0 || line_end < line_start {
        return None;
    }
    Some(GraphSourceSpan {
        raw: raw.to_string(),
        chunk_id: ChunkId(chunk_id.to_string()),
        line_start,
        line_end,
    })
}

fn metadata_string(metadata: Option<&serde_json::Value>, key: &str) -> Option<String> {
    metadata
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn node_label(node: &GraphNode) -> String {
    node.label
        .as_deref()
        .unwrap_or(node.external_id.as_str())
        .trim()
        .to_string()
}

fn canonical_entity_key(entity_type: &str, label: &str) -> String {
    format!("{}:{}", lookup_key(entity_type), lookup_key(label))
}

fn lookup_key(value: &str) -> String {
    let trimmed = value.trim();
    let normalized = normalize_key(trimmed);
    if normalized.is_empty() && !trimmed.is_empty() {
        format!("raw:{}", &hex_sha256(trimmed.as_bytes())[..16])
    } else {
        normalized
    }
}

fn normalize_key(value: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            out.extend(ch.to_lowercase());
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    out.trim().to_string()
}

fn stable_community_id(node_ids: &[GraphNodeId]) -> String {
    let key = node_ids
        .iter()
        .map(|node_id| node_id.0.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    format!("community-{}", &hex_sha256(key.as_bytes())[..16])
}

fn bounded_nonzero(value: usize, hard_cap: usize, default_value: usize) -> usize {
    let value = if value == 0 { default_value } else { value };
    value.min(hard_cap)
}

fn bounded_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn optional_node_key(node_id: &Option<GraphNodeId>) -> String {
    node_id.as_ref().map(|id| id.0.clone()).unwrap_or_default()
}

fn optional_edge_key(edge_id: &Option<GraphEdgeId>) -> String {
    edge_id.as_ref().map(|id| id.0.clone()).unwrap_or_default()
}

struct CanonicalEntityBuilder {
    key: String,
    entity_type: String,
    labels: BTreeSet<String>,
    node_ids: BTreeSet<GraphNodeId>,
    source_spans: BTreeSet<GraphSourceSpan>,
}

impl CanonicalEntityBuilder {
    fn new(key: String, entity_type: String, label: String) -> Self {
        let mut labels = BTreeSet::new();
        labels.insert(label);
        Self {
            key,
            entity_type,
            labels,
            node_ids: BTreeSet::new(),
            source_spans: BTreeSet::new(),
        }
    }

    fn finish(self) -> CanonicalEntity {
        let label = self
            .labels
            .iter()
            .min_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)))
            .cloned()
            .unwrap_or_default();
        CanonicalEntity {
            id: format!(
                "canonical-entity-{}",
                &hex_sha256(self.key.as_bytes())[..16]
            ),
            key: self.key,
            entity_type: self.entity_type,
            label,
            node_ids: self.node_ids.into_iter().collect(),
            source_spans: self.source_spans.into_iter().collect(),
        }
    }
}

struct UnionFind {
    parent: HashMap<String, String>,
}

impl UnionFind {
    fn new<I>(items: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let parent = items
            .into_iter()
            .map(|item| (item.clone(), item))
            .collect::<HashMap<_, _>>();
        Self { parent }
    }

    fn find(&mut self, item: &str) -> String {
        let parent = self
            .parent
            .get(item)
            .cloned()
            .unwrap_or_else(|| item.to_string());
        if parent == item {
            return parent;
        }
        let root = self.find(&parent);
        self.parent.insert(item.to_string(), root.clone());
        root
    }

    fn union(&mut self, left: &str, right: &str) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root == right_root {
            return;
        }
        match left_root.cmp(&right_root) {
            Ordering::Less | Ordering::Equal => {
                self.parent.insert(right_root, left_root);
            }
            Ordering::Greater => {
                self.parent.insert(left_root, right_root);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::store::Store;
    use crate::types::{
        Chunk, ChunkType, EdgeType, EvidenceKind, Source, SourceLocator, SourceStatus,
    };

    #[path = "backing.rs"]
    mod backing_tests;
    #[path = "evidence_lookup.rs"]
    mod evidence_lookup_tests;
    #[path = "report_artifact.rs"]
    mod report_artifact_tests;

    #[test]
    fn canonicalization_groups_entities_stably() {
        let source = SourceId("src".into());
        let first = generated_entity(&source, "Verbatim Core", "component", "chunk-a:1-1");
        let mut second = generated_entity(&source, "verbatim-core", "component", "chunk-a:1-1");
        second.id = GraphNodeId::new(
            &source,
            GraphNodeKind::GeneratedEntity,
            "generated_entity:component:verbatim-core-alt",
        );

        let left = canonicalize_entities(&[first.clone(), second.clone()]);
        let right = canonicalize_entities(&[second, first]);

        assert_eq!(left, right);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].entity_type, "component");
        assert_eq!(left[0].source_spans[0].raw, "chunk-a:1-1");
    }

    #[test]
    fn community_detection_has_stable_ids_and_ordering() {
        let source = SourceId("src".into());
        let alpha = generated_entity(&source, "Alpha", "concept", "chunk-a:1-1");
        let beta = generated_entity(&source, "Beta", "concept", "chunk-a:1-1");
        let gamma = generated_entity(&source, "Gamma", "concept", "chunk-b:1-1");
        let edge = generated_edge(&source, &alpha, &beta, "supports", "chunk-a:1-1");

        let left = detect_communities(
            &[alpha.clone(), beta.clone(), gamma.clone()],
            std::slice::from_ref(&edge),
        );
        let right = detect_communities(&[gamma, beta, alpha], &[edge]);

        assert_eq!(left, right);
        assert_eq!(left.len(), 2);
        assert!(left.iter().any(|community| community.node_ids.len() == 2));
        assert!(left.iter().any(|community| community.node_ids.len() == 1));
    }

    #[test]
    fn canonicalization_keeps_distinct_non_ascii_entities() {
        let source = SourceId("src".into());
        let volcano = generated_entity(&source, "火山", "concept", "chunk-a:1-1");
        let climate = generated_entity(&source, "气候", "concept", "chunk-b:1-1");

        let entities = canonicalize_entities(&[volcano.clone(), climate.clone()]);
        let communities = detect_communities(&[volcano, climate], &[]);

        assert_eq!(entities.len(), 2);
        assert_eq!(communities.len(), 2);
        assert!(communities
            .iter()
            .all(|community| community.node_ids.len() == 1));
    }

    #[test]
    fn community_detection_links_non_ascii_claim_subjects() {
        let source = SourceId("src".into());
        let volcano = generated_entity(&source, "火山", "concept", "chunk-a:1-1");
        let climate = generated_entity(&source, "气候", "concept", "chunk-b:1-1");
        let claim = generated_claim(&source, "火山喷发释放岩浆。", "火山", "岩浆", "chunk-a:1-1");

        let communities =
            detect_communities(&[volcano.clone(), climate.clone(), claim.clone()], &[]);
        let volcano_community = communities
            .iter()
            .find(|community| community.node_ids.contains(&volcano.id))
            .expect("volcano community exists");

        assert_eq!(communities.len(), 2);
        assert!(volcano_community.node_ids.contains(&claim.id));
        assert!(!volcano_community.node_ids.contains(&climate.id));
    }

    #[test]
    fn reports_retain_evidence_and_drop_evidence_free_claims() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let chunk = insert_chunk(&store, &source, "chunk-a", "Alpha supports Beta.");
        let claim_with_evidence = generated_claim(
            &source.id,
            "Alpha supports Beta.",
            "Alpha",
            "Beta",
            "chunk-a:1-1",
        );
        let claim_without_evidence = GraphNode {
            metadata: Some(json!({
                "origin": "llm_generated",
                "graph_data_kind": "claim",
                "claim": "Evidence-free claim",
                "subject": "Alpha",
                "object": "Beta",
                "source_spans": []
            })),
            ..generated_claim(
                &source.id,
                "Evidence-free claim",
                "Alpha",
                "Beta",
                "chunk-a:1-1",
            )
        };
        let community = GraphCommunity {
            id: "community-test".into(),
            node_ids: vec![
                claim_with_evidence.id.clone(),
                claim_without_evidence.id.clone(),
            ],
            edge_ids: Vec::new(),
        };
        let config = enabled_config();

        let reports = build_community_reports(
            &store,
            &[claim_with_evidence, claim_without_evidence],
            &[],
            &[community],
            &config,
        )
        .unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].claims.len(), 1);
        assert_eq!(reports[0].claims[0].text, "Alpha supports Beta.");
        assert_eq!(
            reports[0].evidence[0].evidence.id,
            chunk.evidence_unit_ids[0]
        );
    }

    #[test]
    fn local_grounded_search_preserves_stored_evidence() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let chunk = insert_chunk(&store, &source, "chunk-a", "Grounded local text.");
        let result = retrieval_result(&store, &chunk);
        let disabled = GraphGlobalSearchConfig::default();
        let service = GraphRagService::new(&store, &disabled);

        let local = service.local_search(&[result]).unwrap();

        assert_eq!(local.hits.len(), 1);
        assert_eq!(local.hits[0].evidence_ids, chunk.evidence_unit_ids);
    }

    #[test]
    fn local_graph_chunk_search_preserves_citations_and_entity_provenance() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let chunk = insert_chunk(&store, &source, "chunk-a", "Alpha source text.");
        let entity = generated_entity(&source.id, "Alpha", "concept", "chunk-a:1-1");
        store
            .upsert_graph_nodes(std::slice::from_ref(&entity))
            .unwrap();
        let result = retrieval_result(&store, &chunk);

        let local = local_graph_chunk_search(&store, &[result]).unwrap();

        assert_eq!(local.hits[0].evidence_ids, chunk.evidence_unit_ids);
        assert_eq!(local.hits[0].entity_node_ids, vec![entity.id]);
    }

    #[test]
    fn global_search_returns_no_hits_for_unrelated_non_empty_query() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let config = enabled_config();
        let reports = climate_billing_reports(&store, &source, &config);

        let hits = search_community_reports("volcano eruption", &reports, &config);

        assert!(hits.is_empty());
    }

    #[test]
    fn global_search_returns_no_hits_for_unrelated_short_query() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let config = enabled_config();
        let reports = climate_billing_reports(&store, &source, &config);

        let hits = search_community_reports("AI", &reports, &config);

        assert!(hits.is_empty());
    }

    #[test]
    fn global_search_returns_no_hits_for_unrelated_non_ascii_query() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let config = enabled_config();
        let reports = climate_billing_reports(&store, &source, &config);

        let hits = search_community_reports("火山", &reports, &config);

        assert!(hits.is_empty());
    }

    #[test]
    fn global_search_matches_non_ascii_substring_query() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let config = enabled_config();
        insert_chunk(&store, &source, "chunk-a", "火山喷发释放岩浆。");
        let claim = generated_claim(
            &source.id,
            "火山喷发释放岩浆。",
            "火山",
            "岩浆",
            "chunk-a:1-1",
        );
        let communities = detect_communities(std::slice::from_ref(&claim), &[]);
        let reports =
            build_community_reports(&store, &[claim], &[], &communities, &config).unwrap();

        let hits = search_community_reports("火山", &reports, &config);

        assert_eq!(hits.len(), 1);
        assert!(hits[0].report.summary.contains("火山喷发释放岩浆"));
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn blank_global_search_keeps_overview_fallback() {
        let store = Store::in_memory().unwrap();
        let source = source("src");
        let config = enabled_config();
        let reports = climate_billing_reports(&store, &source, &config);

        let hits = search_community_reports("  ", &reports, &config);

        assert_eq!(hits.len(), reports.len());
        assert!(hits.iter().all(|hit| hit.score > 0.0));
    }

    fn source(id: &str) -> Source {
        Source {
            id: SourceId(id.into()),
            path: std::path::PathBuf::from(format!("/tmp/{id}.txt")),
            hash: format!("hash-{id}"),
            status: SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        }
    }

    fn insert_chunk(store: &Store, source: &Source, chunk_id: &str, text: &str) -> Chunk {
        if store.get_source(&source.id).unwrap().is_none() {
            store.add_source(source).unwrap();
        }
        let evidence = EvidenceUnit {
            id: EvidenceId(format!("ev-{chunk_id}")),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source.path.to_string_lossy().into_owned(),
                line_start: 1,
                line_end: None,
            },
            text: text.into(),
            text_hash: format!("hash-{chunk_id}"),
            heading_path: Vec::new(),
            language: None,
            position: 0,
        };
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: text.into(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: vec![evidence.id.clone()],
        };
        store.bulk_insert_evidence(&[evidence]).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        store
            .link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])
            .unwrap();
        chunk
    }

    fn retrieval_result(store: &Store, chunk: &Chunk) -> RetrievalResult {
        let evidence = chunk
            .evidence_unit_ids
            .iter()
            .filter_map(|id| store.get_evidence(id).unwrap())
            .collect::<Vec<_>>();
        RetrievalResult {
            chunk_id: chunk.id.clone(),
            score: 1.0,
            chunk: chunk.clone(),
            evidence_units: evidence,
            provenance: crate::types::RetrievalProvenance::seed(
                1,
                chunk.id.clone(),
                chunk.source_id.clone(),
            ),
        }
    }

    fn climate_billing_reports(
        store: &Store,
        source: &Source,
        config: &GraphGlobalSearchConfig,
    ) -> Vec<CommunityReport> {
        insert_chunk(store, source, "chunk-a", "Climate evidence.");
        insert_chunk(store, source, "chunk-b", "Billing invoice evidence.");
        let climate = generated_claim(
            &source.id,
            "Climate reports discuss rainfall trends.",
            "Climate",
            "Rainfall",
            "chunk-a:1-1",
        );
        let billing = generated_claim(
            &source.id,
            "Billing reports discuss invoice reconciliation.",
            "Billing",
            "Invoices",
            "chunk-b:1-1",
        );
        let communities = detect_communities(&[climate.clone(), billing.clone()], &[]);
        build_community_reports(store, &[climate, billing], &[], &communities, config).unwrap()
    }

    fn generated_entity(
        source_id: &SourceId,
        label: &str,
        entity_type: &str,
        source_span: &str,
    ) -> GraphNode {
        let external_id = format!("generated_entity:{entity_type}:{}", normalize_key(label));
        GraphNode {
            id: GraphNodeId::new(source_id, GraphNodeKind::GeneratedEntity, &external_id),
            source_id: source_id.clone(),
            kind: GraphNodeKind::GeneratedEntity,
            external_id,
            label: Some(label.into()),
            locator: None,
            ordinal: None,
            metadata: Some(json!({
                "origin": "llm_generated",
                "graph_data_kind": "entity",
                "entity_type": entity_type,
                "description": format!("{label} appears in source evidence."),
                "source_spans": [source_span]
            })),
        }
    }

    fn generated_claim(
        source_id: &SourceId,
        claim: &str,
        subject: &str,
        object: &str,
        source_span: &str,
    ) -> GraphNode {
        let external_id = format!("generated_claim:{}", &hex_sha256(claim.as_bytes())[..16]);
        GraphNode {
            id: GraphNodeId::new(source_id, GraphNodeKind::GeneratedClaim, &external_id),
            source_id: source_id.clone(),
            kind: GraphNodeKind::GeneratedClaim,
            external_id,
            label: Some(claim.into()),
            locator: None,
            ordinal: None,
            metadata: Some(json!({
                "origin": "llm_generated",
                "graph_data_kind": "claim",
                "claim": claim,
                "subject": subject,
                "predicate": "mentions",
                "object": object,
                "source_spans": [source_span]
            })),
        }
    }

    fn generated_edge(
        source_id: &SourceId,
        from: &GraphNode,
        to: &GraphNode,
        relationship_type: &str,
        source_span: &str,
    ) -> GraphEdge {
        GraphEdge {
            id: GraphEdgeId::new(
                source_id,
                EdgeType::GeneratedSupports,
                &from.id,
                &to.id,
                Some(0),
            ),
            source_id: source_id.clone(),
            edge_type: EdgeType::GeneratedSupports,
            from_node_id: from.id.clone(),
            to_node_id: to.id.clone(),
            ordinal: Some(0),
            weight: Some(1.0),
            metadata: Some(json!({
                "origin": "llm_generated",
                "graph_data_kind": "relationship",
                "relationship_type": relationship_type,
                "source": from.label.clone().unwrap_or_default(),
                "target": to.label.clone().unwrap_or_default(),
                "description": "The relationship is supported by source evidence.",
                "source_spans": [source_span]
            })),
        }
    }

    fn enabled_config() -> GraphGlobalSearchConfig {
        GraphGlobalSearchConfig {
            enabled: true,
            ..GraphGlobalSearchConfig::default()
        }
    }
}
