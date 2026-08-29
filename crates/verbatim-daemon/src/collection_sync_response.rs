use verbatim_core::collection::{CollectionStatus, CollectionSyncReport};

fn collection_sync_response(
    collection_name: String,
    report: CollectionSyncReport,
) -> Result<Json<CollectionSyncResponse>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(
        CollectionSyncResponse::new(collection_name.clone(), report)
            .map_err(|e| collection_error(&collection_name, e))?,
    ))
}

fn collection_status_response(
    collection_name: &str,
    status: CollectionStatus,
) -> Result<Json<CollectionStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    Ok(Json(
        CollectionStatusResponse::new(collection_name, status)
            .map_err(|e| collection_error(collection_name, e))?,
    ))
}
