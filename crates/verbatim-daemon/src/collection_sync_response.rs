use verbatim_core::collection::CollectionSyncReport;

fn collection_sync_response(
    collection_name: String,
    report: CollectionSyncReport,
) -> Result<Json<CollectionSyncResponse>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(
        CollectionSyncResponse::new(collection_name.clone(), report)
            .map_err(|e| collection_error(&collection_name, e))?,
    ))
}
