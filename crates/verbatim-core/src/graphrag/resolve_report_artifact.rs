use super::*;

impl GraphRagService<'_> {
    /// Reconstruct a derived report artifact from the current graph state.
    pub fn resolve_report_artifact(
        &self,
        id: &ReportArtifactId,
    ) -> Result<Option<ReportArtifactManifest>> {
        for source_filter in std::iter::once(None).chain(
            self.store
                .list_sources()?
                .iter()
                .map(|source| Some(&source.id)),
        ) {
            if let Some(report) =
                self.community_reports(source_filter)?
                    .into_iter()
                    .find(|report| {
                        ReportArtifactId::new(&report.id).is_ok_and(|report_id| report_id == *id)
                    })
            {
                return Ok(Some(ReportArtifactManifest {
                    id: id.clone(),
                    generation: report.generation.clone(),
                    content_hash: report.content_hash.clone(),
                    report,
                }));
            }
        }
        Ok(None)
    }
}
