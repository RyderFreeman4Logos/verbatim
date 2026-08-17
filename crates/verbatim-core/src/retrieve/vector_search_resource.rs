//! Optional admission control for dense vector search.
//!
//! Injecting an [`ObservableResource`] bounds dense backend work and holds its
//! permit until that work completes. Omitting the resource leaves standalone
//! [`RetrievalPipeline`] instances unbounded; production daemon construction
//! injects its configured vector-search resource.

use anyhow::{Context, Result};

use super::{source_filter::single_source_filter, RetrievalPipeline};
use crate::overfetch::{
    AdaptiveOverfetchPolicy, AdaptiveOverfetchPolicyFields, OverfetchError, StrictFilterSupport,
};
use crate::resource::{ObservableResource, ResourcePermit};
use crate::retrieval_telemetry::CandidateCounters;
#[cfg(feature = "qdrant")]
use crate::types::EmbeddingProfileId;
use crate::types::{
    ChunkId, RetrievalDenseVectorPath, RetrievalLocalSpansMs, SourceId, VectorIndexResidency,
};
use std::collections::HashSet;
use std::sync::Arc;

const MAX_SCOPED_DENSE_CANDIDATE_K: u32 = 256;
const SCOPED_DENSE_OVERFETCH_GROWTH_FACTOR: u8 = 2;
const SCOPED_DENSE_OVERFETCH_MAX_ATTEMPTS: u8 = 9;

impl RetrievalPipeline<'_> {
    /// Bound dense backend work with the supplied observable resource.
    ///
    /// The permit covers the complete dense operation. Without this optional
    /// injection, standalone pipelines do not apply dense-search admission
    /// control; production daemon construction supplies its configured resource.
    pub fn with_vector_search_resource(mut self, resource: Arc<ObservableResource>) -> Self {
        self.vector_search_resource = Some(resource);
        self
    }

    pub(super) async fn acquire_vector_search_permit(&self) -> Result<Option<ResourcePermit>> {
        match &self.vector_search_resource {
            Some(resource) => resource
                .acquire()
                .await
                .context("acquire vector search resource")
                .map(Some),
            None => Ok(None),
        }
    }

    fn local_strict_filter_support(
        &self,
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> StrictFilterSupport {
        if source_filter.is_none()
            || self.vector_residency == VectorIndexResidency::LowMemory
            || (single_source_filter(source_filter).is_some()
                && self.vector_index.supports_source_filter())
        {
            return StrictFilterSupport::Native;
        }

        // A caller-fixed K that already covers a small index needs no adaptive widening.
        if self.vector_index.len() <= top_k {
            return StrictFilterSupport::Native;
        }
        let Ok(initial_candidate_k) = u32::try_from(top_k) else {
            return StrictFilterSupport::Unsupported;
        };
        AdaptiveOverfetchPolicy::new(AdaptiveOverfetchPolicyFields {
            initial_candidate_k,
            max_candidate_k: MAX_SCOPED_DENSE_CANDIDATE_K,
            growth_factor: SCOPED_DENSE_OVERFETCH_GROWTH_FACTOR,
            max_attempts: SCOPED_DENSE_OVERFETCH_MAX_ATTEMPTS,
        })
        .map(StrictFilterSupport::Adaptive)
        .unwrap_or(StrictFilterSupport::Unsupported)
    }

    fn bounded_local_dense_search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
        candidate_counters: &mut CandidateCounters,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if top_k == 0 || query_vec.is_empty() {
            return self
                .with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter));
        }
        let policy = match self.local_strict_filter_support(top_k, source_filter) {
            StrictFilterSupport::Native => {
                return self
                    .with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter));
            }
            StrictFilterSupport::Adaptive(policy) => policy,
            StrictFilterSupport::Unsupported => {
                return Err(OverfetchError::UnsupportedStrictFilter.into());
            }
        };

        let support = StrictFilterSupport::Adaptive(policy);
        let corpus_size = u64::try_from(self.vector_index.len()).unwrap_or(u64::MAX);
        let mut previous_candidate_k = None;
        for attempt in 0..policy.max_attempts {
            let Ok(candidate_k) = support.candidate_k_for_attempt(
                policy.initial_candidate_k,
                MAX_SCOPED_DENSE_CANDIDATE_K,
                corpus_size,
                attempt,
            ) else {
                return Err(OverfetchError::UnsupportedStrictFilter.into());
            };
            if candidate_k == 0 || previous_candidate_k == Some(candidate_k) {
                return Err(OverfetchError::UnsupportedStrictFilter.into());
            }
            previous_candidate_k = Some(candidate_k);

            let candidates = self.with_read_permit(|| {
                self.local_dense_search(query_vec, candidate_k as usize, source_filter)
            })?;
            let hits = self.with_read_permit(|| {
                self.valid_dense_hits(candidates, top_k, source_filter, None, candidate_counters)
            })?;
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Err(OverfetchError::UnsupportedStrictFilter.into())
    }

    pub(super) async fn dense_search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
        candidate_counters: &mut CandidateCounters,
        local_spans_ms: &mut RetrievalLocalSpansMs,
    ) -> Result<(Vec<(ChunkId, f32)>, RetrievalDenseVectorPath)> {
        local_spans_ms.vector_queue_wait_ms = None;
        local_spans_ms.vector_service_ms = None;
        let permit = self.acquire_vector_search_permit().await?;
        #[cfg(feature = "qdrant")]
        let result = if top_k == 0 || query_vec.is_empty() {
            self.with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter))
                .map(|hits| (hits, self.dense_vector_path()))
        } else if let Some(qdrant) = &self.qdrant {
            let default_profile_id;
            let profile_id = match &self.required_profile_id {
                Some(profile_id) => profile_id,
                None => {
                    default_profile_id = EmbeddingProfileId::default_profile();
                    &default_profile_id
                }
            };
            let profile_generation =
                self.with_read_permit(|| self.store.index_generation_for_profile(profile_id))?;
            match qdrant
                .search(profile_id, query_vec, top_k, source_filter)
                .await
            {
                Ok(results) => {
                    let mut hits = self.with_read_permit(|| {
                        self.valid_dense_hits(
                            results,
                            top_k,
                            source_filter,
                            Some((profile_id, profile_generation)),
                            candidate_counters,
                        )
                    })?;
                    if hits.len() < top_k {
                        let local_results = self.bounded_local_dense_search(
                            query_vec,
                            top_k,
                            source_filter,
                            candidate_counters,
                        )?;
                        let local_hits = self.with_read_permit(|| {
                            self.valid_dense_hits(
                                local_results,
                                top_k,
                                source_filter,
                                None,
                                candidate_counters,
                            )
                        })?;
                        let missing = top_k - hits.len();
                        let mut seen = hits
                            .iter()
                            .map(|(chunk_id, _)| chunk_id.clone())
                            .collect::<HashSet<_>>();
                        hits.extend(
                            local_hits
                                .into_iter()
                                .filter(|(chunk_id, _)| seen.insert(chunk_id.clone()))
                                .take(missing),
                        );
                    }
                    Ok((hits, RetrievalDenseVectorPath::Qdrant))
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "qdrant search failed; falling back to local dense index"
                    );
                    let local_results = self.bounded_local_dense_search(
                        query_vec,
                        top_k,
                        source_filter,
                        candidate_counters,
                    )?;
                    self.with_read_permit(|| {
                        self.valid_dense_hits(
                            local_results,
                            top_k,
                            source_filter,
                            None,
                            candidate_counters,
                        )
                    })
                    .map(|hits| (hits, self.dense_vector_path()))
                }
            }
        } else {
            self.bounded_local_dense_search(query_vec, top_k, source_filter, candidate_counters)
                .map(|hits| (hits, self.dense_vector_path()))
        };
        #[cfg(not(feature = "qdrant"))]
        let result = self
            .bounded_local_dense_search(query_vec, top_k, source_filter, candidate_counters)
            .map(|hits| (hits, self.dense_vector_path()));
        let output = result?;
        if let Some(permit) = permit.as_ref() {
            local_spans_ms.vector_queue_wait_ms = Some(permit.queue_wait_ms());
            local_spans_ms.vector_service_ms = Some(permit.service_ms());
        }
        Ok(output)
    }
}
