//! Optional admission control for dense vector search.
//!
//! Injecting an [`ObservableResource`] bounds dense backend work and holds its
//! permit until that work completes. Omitting the resource leaves standalone
//! [`RetrievalPipeline`] instances unbounded; production daemon construction
//! injects its configured vector-search resource.

use anyhow::{Context, Result};

#[cfg(feature = "qdrant")]
use super::single_source_filter;
use super::RetrievalPipeline;
use crate::resource::{ObservableResource, ResourcePermit};
use crate::retrieval_telemetry::CandidateCounters;
#[cfg(feature = "qdrant")]
use crate::types::EmbeddingProfileId;
use crate::types::{ChunkId, RetrievalDenseVectorPath, RetrievalLocalSpansMs, SourceId};
use std::collections::HashSet;
use std::sync::Arc;

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
        let result = if let Some(qdrant) = &self.qdrant {
            let local_results =
                self.with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter))?;
            if local_results.is_empty() {
                Ok((local_results, self.dense_vector_path()))
            } else {
                let default_profile_id;
                let profile_id = match &self.required_profile_id {
                    Some(profile_id) => profile_id,
                    None => {
                        default_profile_id = EmbeddingProfileId::default_profile();
                        &default_profile_id
                    }
                };
                let qdrant_source_filter = single_source_filter(source_filter);
                let profile_generation =
                    self.with_read_permit(|| self.store.index_generation_for_profile(profile_id))?;
                match qdrant
                    .search(profile_id, query_vec, top_k, qdrant_source_filter)
                    .await
                {
                    Ok(results) => self
                        .with_read_permit(|| {
                            self.merge_preferred_dense_hits(
                                (profile_id, profile_generation),
                                results,
                                local_results,
                                top_k,
                                source_filter,
                                candidate_counters,
                            )
                        })
                        .map(|hits| (hits, RetrievalDenseVectorPath::Qdrant)),
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "qdrant search failed; falling back to local dense index"
                        );
                        self.with_read_permit(|| {
                            self.valid_dense_hits(
                                local_results,
                                top_k,
                                source_filter,
                                candidate_counters,
                            )
                        })
                        .map(|hits| (hits, self.dense_vector_path()))
                    }
                }
            }
        } else {
            self.with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter))
                .map(|hits| (hits, self.dense_vector_path()))
        };
        #[cfg(not(feature = "qdrant"))]
        let result = {
            let _ = candidate_counters;
            self.with_read_permit(|| self.local_dense_search(query_vec, top_k, source_filter))
                .map(|hits| (hits, self.dense_vector_path()))
        };
        let output = result?;
        if let Some(permit) = permit.as_ref() {
            local_spans_ms.vector_queue_wait_ms = Some(permit.queue_wait_ms());
            local_spans_ms.vector_service_ms = Some(permit.service_ms());
        }
        Ok(output)
    }
}
