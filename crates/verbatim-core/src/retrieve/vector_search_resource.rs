//! Optional admission control for dense vector search.
//!
//! Injecting an [`ObservableResource`] bounds dense backend work and holds its
//! permit until that work completes. Omitting the resource leaves standalone
//! [`RetrievalPipeline`] instances unbounded; production daemon construction
//! injects its configured vector-search resource.

use anyhow::{Context, Result};

use super::RetrievalPipeline;
use crate::resource::{ObservableResource, ResourcePermit};
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
}
