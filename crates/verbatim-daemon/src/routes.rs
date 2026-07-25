//! Daemon HTTP route inventory and router construction.
//!
//! First walking-skeleton slice for the daemon module split (#342). This module
//! owns the route table only; handler bodies remain in the crate root.

use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use verbatim_core::api::CollectionApiEndpoint;

use super::auth_middleware;
use super::SharedState;

/// Number of `.route(...)` registrations installed by [`build_router`].
///
/// Keep this in lock-step with the route inventory test below. Adding or
/// removing a route registration without updating both is a bug.
pub(crate) const ROUTE_REGISTRATION_COUNT: usize = 38;

// Keep the inventory size live in non-test builds so clippy dead_code cannot
// paper over drift between `build_router` and the route inventory tests.
const _: usize = ROUTE_REGISTRATION_COUNT;

/// Build the daemon HTTP router for the given shared state.
///
/// Handlers stay in the crate root; this function only composes the inventory
/// and middleware stack.
pub(crate) fn build_router(state: SharedState) -> Router {
    let auth_config = state
        .runtime_config
        .read()
        .map(|runtime| runtime.config.daemon.auth.clone())
        .unwrap_or_default();
    let auth_state = auth_middleware::AuthMiddlewareState::from_runtime_config(auth_config);

    Router::new()
        .route("/api/health", get(super::health))
        .route("/api/config", get(super::get_config))
        .route("/api/sources", post(super::add_source))
        .route("/api/sources", get(super::list_sources))
        .route("/api/sources/{id}", get(super::get_source))
        .route("/api/sources/{id}", delete(super::delete_source))
        .route("/api/deletions/reports", get(super::list_deletion_reports))
        .route("/api/sources/check", post(super::check_stale))
        .route(
            CollectionApiEndpoint::CreateCollection.path_template(),
            post(super::create_collection),
        )
        .route(
            CollectionApiEndpoint::ListCollections.path_template(),
            get(super::list_collections),
        )
        .route(
            CollectionApiEndpoint::GetCollection.path_template(),
            get(super::get_collection),
        )
        .route(
            CollectionApiEndpoint::DeleteCollection.path_template(),
            delete(super::delete_collection),
        )
        .route(
            CollectionApiEndpoint::AddCollectionRoot.path_template(),
            post(super::add_collection_root),
        )
        .route(
            CollectionApiEndpoint::SyncCollection.path_template(),
            post(super::sync_collection),
        )
        .route(
            CollectionApiEndpoint::CollectionStatus.path_template(),
            get(super::collection_status),
        )
        .route(
            CollectionApiEndpoint::ListCollectionWatcherStatuses.path_template(),
            get(super::list_collection_watcher_statuses),
        )
        .route(
            CollectionApiEndpoint::CollectionWatcherStatus.path_template(),
            get(super::collection_watcher_status).put(super::update_collection_watcher),
        )
        .route("/api/ingest", post(super::ingest_all))
        .route("/api/ingest/{id}", post(super::ingest_one))
        .route("/api/reindex", post(super::reindex))
        .route("/api/index/status", get(super::index_status))
        .route("/api/index/gc", post(super::index_gc))
        .route(
            "/api/index/profiles/delete",
            post(super::index_delete_profile),
        )
        .route(
            "/api/index/vector-json/cleanup",
            post(super::vector_json_cleanup),
        )
        .route("/api/ask", post(super::ask))
        .route("/api/ask/stream", post(super::ask_stream))
        .route("/api/retrieve", post(super::retrieve))
        .route("/api/tasks/ask", post(super::submit_ask_task))
        .route("/api/tasks/ingest", post(super::submit_ingest_task))
        .route("/api/tasks/reindex", post(super::submit_reindex_task))
        .route("/api/tasks", get(super::list_tasks_handler))
        .route("/api/tasks/{id}", get(super::show_task))
        .route("/api/tasks/{id}/profile", get(super::task_profile_handler))
        .route(
            "/api/tasks/{id}/events",
            get(super::list_task_events_handler),
        )
        .route("/api/tasks/{id}/wait", get(super::wait_task))
        .route("/api/tasks/{id}/cancel", post(super::cancel_task_handler))
        .route("/api/tasks/{id}/resume", post(super::resume_task_handler))
        .route("/api/evidence/{eid}", get(super::get_evidence))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            super::track_http_activity,
        ))
        .layer(middleware::from_fn_with_state(
            auth_state,
            auth_middleware::authenticate_request,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path templates registered via `.route(...)` in [`build_router`].
    ///
    /// Duplicate path strings are intentional when the same path is registered
    /// more than once (separate method sets). Keep this list ordered and
    /// length-aligned with the router construction above.
    const ROUTE_PATH_TEMPLATES: &[&str] = &[
        "/api/health",
        "/api/config",
        "/api/sources",
        "/api/sources",
        "/api/sources/{id}",
        "/api/sources/{id}",
        "/api/deletions/reports",
        "/api/sources/check",
        "/api/collections",
        "/api/collections",
        "/api/collections/{name}",
        "/api/collections/{name}",
        "/api/collections/{name}/roots",
        "/api/collections/{name}/sync",
        "/api/collections/{name}/status",
        "/api/collections/watchers/status",
        "/api/collections/{name}/watcher",
        "/api/ingest",
        "/api/ingest/{id}",
        "/api/reindex",
        "/api/index/status",
        "/api/index/gc",
        "/api/index/profiles/delete",
        "/api/index/vector-json/cleanup",
        "/api/ask",
        "/api/ask/stream",
        "/api/retrieve",
        "/api/tasks/ask",
        "/api/tasks/ingest",
        "/api/tasks/reindex",
        "/api/tasks",
        "/api/tasks/{id}",
        "/api/tasks/{id}/profile",
        "/api/tasks/{id}/events",
        "/api/tasks/{id}/wait",
        "/api/tasks/{id}/cancel",
        "/api/tasks/{id}/resume",
        "/api/evidence/{eid}",
    ];

    #[test]
    fn route_inventory_matches_registration_count() {
        assert_eq!(
            ROUTE_PATH_TEMPLATES.len(),
            ROUTE_REGISTRATION_COUNT,
            "route inventory length must match ROUTE_REGISTRATION_COUNT"
        );
        assert_eq!(
            ROUTE_PATH_TEMPLATES.len(),
            38,
            "expected 38 daemon route registrations in the first #342 slice"
        );
    }

    #[test]
    fn collection_route_templates_match_api_contract() {
        assert_eq!(
            CollectionApiEndpoint::CreateCollection.path_template(),
            "/api/collections"
        );
        assert_eq!(
            CollectionApiEndpoint::ListCollections.path_template(),
            "/api/collections"
        );
        assert_eq!(
            CollectionApiEndpoint::GetCollection.path_template(),
            "/api/collections/{name}"
        );
        assert_eq!(
            CollectionApiEndpoint::DeleteCollection.path_template(),
            "/api/collections/{name}"
        );
        assert_eq!(
            CollectionApiEndpoint::AddCollectionRoot.path_template(),
            "/api/collections/{name}/roots"
        );
        assert_eq!(
            CollectionApiEndpoint::SyncCollection.path_template(),
            "/api/collections/{name}/sync"
        );
        assert_eq!(
            CollectionApiEndpoint::CollectionStatus.path_template(),
            "/api/collections/{name}/status"
        );
        assert_eq!(
            CollectionApiEndpoint::ListCollectionWatcherStatuses.path_template(),
            "/api/collections/watchers/status"
        );
        assert_eq!(
            CollectionApiEndpoint::CollectionWatcherStatus.path_template(),
            "/api/collections/{name}/watcher"
        );
    }
}
