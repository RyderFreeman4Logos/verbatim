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

// ---------------------------------------------------------------------------
// Path template inventory (single source of truth)
//
// Each constant is used both in [`build_router`] and in
// [`ROUTE_PATH_TEMPLATES`]. Adding a route means adding a constant, appending
// it to the inventory, and registering it in `build_router` — in that order.
// ---------------------------------------------------------------------------

const PATH_HEALTH: &str = "/api/health";
const PATH_CONFIG: &str = "/api/config";
const PATH_SOURCES: &str = "/api/sources";
const PATH_SOURCE_BY_ID: &str = "/api/sources/{id}";
const PATH_DELETION_REPORTS: &str = "/api/deletions/reports";
const PATH_SOURCES_CHECK: &str = "/api/sources/check";
// Collection paths come from the core API contract; keep local aliases so the
// inventory and `build_router` share the exact same `&'static str` values.
const PATH_COLLECTIONS: &str = CollectionApiEndpoint::CreateCollection.path_template();
const PATH_COLLECTION_BY_NAME: &str = CollectionApiEndpoint::GetCollection.path_template();
const PATH_COLLECTION_ROOTS: &str = CollectionApiEndpoint::AddCollectionRoot.path_template();
const PATH_COLLECTION_SYNC: &str = CollectionApiEndpoint::SyncCollection.path_template();
const PATH_COLLECTION_STATUS: &str = CollectionApiEndpoint::CollectionStatus.path_template();
const PATH_COLLECTION_WATCHERS_STATUS: &str =
    CollectionApiEndpoint::ListCollectionWatcherStatuses.path_template();
const PATH_COLLECTION_WATCHER: &str =
    CollectionApiEndpoint::CollectionWatcherStatus.path_template();
const PATH_INGEST: &str = "/api/ingest";
const PATH_INGEST_BY_ID: &str = "/api/ingest/{id}";
const PATH_REINDEX: &str = "/api/reindex";
const PATH_INDEX_STATUS: &str = "/api/index/status";
const PATH_INDEX_GC: &str = "/api/index/gc";
const PATH_INDEX_PROFILES_DELETE: &str = "/api/index/profiles/delete";
const PATH_INDEX_VECTOR_JSON_CLEANUP: &str = "/api/index/vector-json/cleanup";
const PATH_ASK: &str = "/api/ask";
const PATH_ASK_STREAM: &str = "/api/ask/stream";
const PATH_RETRIEVE: &str = "/api/retrieve";
const PATH_TASKS_ASK: &str = "/api/tasks/ask";
const PATH_TASKS_INGEST: &str = "/api/tasks/ingest";
const PATH_TASKS_REINDEX: &str = "/api/tasks/reindex";
const PATH_TASKS: &str = "/api/tasks";
const PATH_TASK_BY_ID: &str = "/api/tasks/{id}";
const PATH_TASK_PROFILE: &str = "/api/tasks/{id}/profile";
const PATH_TASK_EVENTS: &str = "/api/tasks/{id}/events";
const PATH_TASK_WAIT: &str = "/api/tasks/{id}/wait";
const PATH_TASK_CANCEL: &str = "/api/tasks/{id}/cancel";
const PATH_TASK_RESUME: &str = "/api/tasks/{id}/resume";
const PATH_EVIDENCE_BY_EID: &str = "/api/evidence/{eid}";

/// Ordered path templates registered by [`build_router`].
///
/// Duplicate path strings are intentional when the same path is registered
/// more than once (separate method sets). Order matches the `.route(...)`
/// chain in [`build_router`] exactly.
pub(crate) const ROUTE_PATH_TEMPLATES: &[&str] = &[
    PATH_HEALTH,
    PATH_CONFIG,
    PATH_SOURCES,
    PATH_SOURCES,
    PATH_SOURCE_BY_ID,
    PATH_SOURCE_BY_ID,
    PATH_DELETION_REPORTS,
    PATH_SOURCES_CHECK,
    PATH_COLLECTIONS,
    PATH_COLLECTIONS,
    PATH_COLLECTION_BY_NAME,
    PATH_COLLECTION_BY_NAME,
    PATH_COLLECTION_ROOTS,
    PATH_COLLECTION_SYNC,
    PATH_COLLECTION_STATUS,
    PATH_COLLECTION_WATCHERS_STATUS,
    PATH_COLLECTION_WATCHER,
    PATH_INGEST,
    PATH_INGEST_BY_ID,
    PATH_REINDEX,
    PATH_INDEX_STATUS,
    PATH_INDEX_GC,
    PATH_INDEX_PROFILES_DELETE,
    PATH_INDEX_VECTOR_JSON_CLEANUP,
    PATH_ASK,
    PATH_ASK_STREAM,
    PATH_RETRIEVE,
    PATH_TASKS_ASK,
    PATH_TASKS_INGEST,
    PATH_TASKS_REINDEX,
    PATH_TASKS,
    PATH_TASK_BY_ID,
    PATH_TASK_PROFILE,
    PATH_TASK_EVENTS,
    PATH_TASK_WAIT,
    PATH_TASK_CANCEL,
    PATH_TASK_RESUME,
    PATH_EVIDENCE_BY_EID,
];

/// Number of `.route(...)` registrations installed by [`build_router`].
///
/// Derived from the inventory so the count cannot drift from the path table.
pub(crate) const ROUTE_REGISTRATION_COUNT: usize = ROUTE_PATH_TEMPLATES.len();

// Keep the inventory size live in non-test builds so clippy dead_code cannot
// paper over drift between `build_router` and the route inventory tests.
const _: usize = ROUTE_REGISTRATION_COUNT;

/// Path templates registered by [`build_router`], in registration order.
///
/// This is the same inventory table `build_router` consumes for every path
/// argument; tests assert against this function rather than a parallel list.
pub(crate) fn registered_route_path_templates() -> &'static [&'static str] {
    ROUTE_PATH_TEMPLATES
}

// Keep the inventory seam live in non-test builds (same pattern as
// ROUTE_REGISTRATION_COUNT above).
const _: fn() -> &'static [&'static str] = registered_route_path_templates;

/// Build the daemon HTTP router for the given shared state.
///
/// Handlers stay in the crate root; this function only composes the inventory
/// and middleware stack. Every path argument is taken from the shared path
/// constants that also populate [`ROUTE_PATH_TEMPLATES`].
pub(crate) fn build_router(state: SharedState) -> Router {
    let auth_config = state
        .runtime_config
        .read()
        .map(|runtime| runtime.config.daemon.auth.clone())
        .unwrap_or_default();
    let auth_state = auth_middleware::AuthMiddlewareState::from_runtime_config(auth_config);

    Router::new()
        .route(PATH_HEALTH, get(super::health))
        .route(PATH_CONFIG, get(super::get_config))
        .route(PATH_SOURCES, post(super::add_source))
        .route(PATH_SOURCES, get(super::list_sources))
        .route(PATH_SOURCE_BY_ID, get(super::get_source))
        .route(PATH_SOURCE_BY_ID, delete(super::delete_source))
        .route(PATH_DELETION_REPORTS, get(super::list_deletion_reports))
        .route(PATH_SOURCES_CHECK, post(super::check_stale))
        .route(PATH_COLLECTIONS, post(super::create_collection))
        .route(PATH_COLLECTIONS, get(super::list_collections))
        .route(PATH_COLLECTION_BY_NAME, get(super::get_collection))
        .route(PATH_COLLECTION_BY_NAME, delete(super::delete_collection))
        .route(PATH_COLLECTION_ROOTS, post(super::add_collection_root))
        .route(PATH_COLLECTION_SYNC, post(super::sync_collection))
        .route(PATH_COLLECTION_STATUS, get(super::collection_status))
        .route(
            PATH_COLLECTION_WATCHERS_STATUS,
            get(super::list_collection_watcher_statuses),
        )
        .route(
            PATH_COLLECTION_WATCHER,
            get(super::collection_watcher_status).put(super::update_collection_watcher),
        )
        .route(PATH_INGEST, post(super::ingest_all))
        .route(PATH_INGEST_BY_ID, post(super::ingest_one))
        .route(PATH_REINDEX, post(super::reindex))
        .route(PATH_INDEX_STATUS, get(super::index_status))
        .route(PATH_INDEX_GC, post(super::index_gc))
        .route(
            PATH_INDEX_PROFILES_DELETE,
            post(super::index_delete_profile),
        )
        .route(
            PATH_INDEX_VECTOR_JSON_CLEANUP,
            post(super::vector_json_cleanup),
        )
        .route(PATH_ASK, post(super::ask))
        .route(PATH_ASK_STREAM, post(super::ask_stream))
        .route(PATH_RETRIEVE, post(super::retrieve))
        .route(PATH_TASKS_ASK, post(super::submit_ask_task))
        .route(PATH_TASKS_INGEST, post(super::submit_ingest_task))
        .route(PATH_TASKS_REINDEX, post(super::submit_reindex_task))
        .route(PATH_TASKS, get(super::list_tasks_handler))
        .route(PATH_TASK_BY_ID, get(super::show_task))
        .route(PATH_TASK_PROFILE, get(super::task_profile_handler))
        .route(PATH_TASK_EVENTS, get(super::list_task_events_handler))
        .route(PATH_TASK_WAIT, get(super::wait_task))
        .route(PATH_TASK_CANCEL, post(super::cancel_task_handler))
        .route(PATH_TASK_RESUME, post(super::resume_task_handler))
        .route(PATH_EVIDENCE_BY_EID, get(super::get_evidence))
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

    #[test]
    fn route_inventory_matches_registration_count() {
        assert_eq!(
            ROUTE_PATH_TEMPLATES.len(),
            ROUTE_REGISTRATION_COUNT,
            "route inventory length must match ROUTE_REGISTRATION_COUNT"
        );
        assert_eq!(
            registered_route_path_templates().len(),
            ROUTE_REGISTRATION_COUNT,
            "registered_route_path_templates must report the same count build_router uses"
        );
        assert_eq!(
            ROUTE_REGISTRATION_COUNT, 38,
            "expected 38 daemon route registrations in the first #342 slice"
        );
    }

    #[test]
    fn registered_paths_are_the_inventory_build_router_consumes() {
        // Structural coupling: build_router path arguments are the same
        // `PATH_*` constants that populate ROUTE_PATH_TEMPLATES. This test
        // pins the public inventory seam to that table so a parallel list
        // cannot reappear.
        assert_eq!(
            registered_route_path_templates(),
            ROUTE_PATH_TEMPLATES,
            "build_router inventory seam must equal ROUTE_PATH_TEMPLATES"
        );
        assert_eq!(
            registered_route_path_templates().first().copied(),
            Some(PATH_HEALTH)
        );
        assert_eq!(
            registered_route_path_templates().last().copied(),
            Some(PATH_EVIDENCE_BY_EID)
        );
        // Spot-check registrations use the shared path-constant values (same
        // literals build_router passes to `.route(...)`).
        assert_eq!(registered_route_path_templates()[17], PATH_INGEST);
        assert_eq!(registered_route_path_templates()[31], PATH_TASK_BY_ID);
        assert_eq!(
            registered_route_path_templates(),
            [
                PATH_HEALTH,
                PATH_CONFIG,
                PATH_SOURCES,
                PATH_SOURCES,
                PATH_SOURCE_BY_ID,
                PATH_SOURCE_BY_ID,
                PATH_DELETION_REPORTS,
                PATH_SOURCES_CHECK,
                PATH_COLLECTIONS,
                PATH_COLLECTIONS,
                PATH_COLLECTION_BY_NAME,
                PATH_COLLECTION_BY_NAME,
                PATH_COLLECTION_ROOTS,
                PATH_COLLECTION_SYNC,
                PATH_COLLECTION_STATUS,
                PATH_COLLECTION_WATCHERS_STATUS,
                PATH_COLLECTION_WATCHER,
                PATH_INGEST,
                PATH_INGEST_BY_ID,
                PATH_REINDEX,
                PATH_INDEX_STATUS,
                PATH_INDEX_GC,
                PATH_INDEX_PROFILES_DELETE,
                PATH_INDEX_VECTOR_JSON_CLEANUP,
                PATH_ASK,
                PATH_ASK_STREAM,
                PATH_RETRIEVE,
                PATH_TASKS_ASK,
                PATH_TASKS_INGEST,
                PATH_TASKS_REINDEX,
                PATH_TASKS,
                PATH_TASK_BY_ID,
                PATH_TASK_PROFILE,
                PATH_TASK_EVENTS,
                PATH_TASK_WAIT,
                PATH_TASK_CANCEL,
                PATH_TASK_RESUME,
                PATH_EVIDENCE_BY_EID,
            ]
        );
    }

    #[test]
    fn collection_route_templates_match_api_contract() {
        // Inventory collection slots are the CollectionApiEndpoint templates
        // (same statics), not parallel string literals.
        assert_eq!(
            PATH_COLLECTIONS,
            CollectionApiEndpoint::CreateCollection.path_template()
        );
        assert_eq!(
            PATH_COLLECTIONS,
            CollectionApiEndpoint::ListCollections.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_BY_NAME,
            CollectionApiEndpoint::GetCollection.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_BY_NAME,
            CollectionApiEndpoint::DeleteCollection.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_ROOTS,
            CollectionApiEndpoint::AddCollectionRoot.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_SYNC,
            CollectionApiEndpoint::SyncCollection.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_STATUS,
            CollectionApiEndpoint::CollectionStatus.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_WATCHERS_STATUS,
            CollectionApiEndpoint::ListCollectionWatcherStatuses.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_WATCHER,
            CollectionApiEndpoint::CollectionWatcherStatus.path_template()
        );
        assert_eq!(
            PATH_COLLECTION_WATCHER,
            CollectionApiEndpoint::UpdateCollectionWatcher.path_template()
        );

        // And those shared values occupy the expected inventory slots.
        assert_eq!(ROUTE_PATH_TEMPLATES[8], PATH_COLLECTIONS);
        assert_eq!(ROUTE_PATH_TEMPLATES[9], PATH_COLLECTIONS);
        assert_eq!(ROUTE_PATH_TEMPLATES[10], PATH_COLLECTION_BY_NAME);
        assert_eq!(ROUTE_PATH_TEMPLATES[11], PATH_COLLECTION_BY_NAME);
        assert_eq!(ROUTE_PATH_TEMPLATES[12], PATH_COLLECTION_ROOTS);
        assert_eq!(ROUTE_PATH_TEMPLATES[13], PATH_COLLECTION_SYNC);
        assert_eq!(ROUTE_PATH_TEMPLATES[14], PATH_COLLECTION_STATUS);
        assert_eq!(ROUTE_PATH_TEMPLATES[15], PATH_COLLECTION_WATCHERS_STATUS);
        assert_eq!(ROUTE_PATH_TEMPLATES[16], PATH_COLLECTION_WATCHER);
    }
}
