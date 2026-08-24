use super::*;

impl GraphRagService<'_> {
    /// Reconstruct a derived report artifact from the current graph state.
    pub fn resolve_report_artifact(
        &self,
        id: &ReportArtifactId,
    ) -> Result<Option<CommunityReport>> {
        Ok(self.community_reports(None)?.into_iter().find(|report| {
            ReportArtifactId::new(&report.id).is_ok_and(|report_id| report_id == *id)
        }))
    }
}
