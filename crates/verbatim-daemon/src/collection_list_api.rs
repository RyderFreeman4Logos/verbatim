//! Collection-list publication boundary.

use super::*;
use verbatim_core::CollectionListResponse;

pub(super) async fn list_collections(
    State(state): State<SharedState>,
) -> Result<Json<CollectionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let collections = with_task_store_read(&state, |store| store.list_collections())
        .await
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;

    CollectionListResponse::new(collections)
        .map(Json)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))
}
