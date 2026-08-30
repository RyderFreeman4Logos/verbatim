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
const PATH_SOURCE_RELOCATIONS: &str = "/api/source-relocations";
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
const PATH_EVIDENCE_BY_EID: &str = "/api/evidence/{*eid}";
const PATH_REPORT_ARTIFACT_BY_ID: &str = "/api/report-artifact/{*id}";

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
    PATH_SOURCE_RELOCATIONS,
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
    PATH_REPORT_ARTIFACT_BY_ID,
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
        .route(PATH_SOURCE_RELOCATIONS, post(super::relocate_source))
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
        .route(PATH_REPORT_ARTIFACT_BY_ID, get(super::get_report_artifact))
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
    use crate::tests::retrieve_test_config;
    use axum::body::{to_bytes, Body};
    use axum::extract::connect_info::ConnectInfo;
    use axum::http::{Method, Request, StatusCode};
    use std::collections::BTreeSet;
    use std::net::SocketAddr;
    use tower::ServiceExt;
    use verbatim_core::deletion::{DeletionOutcome, DeletionProduct, PersistedDeletionReport};
    use verbatim_core::ingest::IngestPipeline;
    use verbatim_core::{CollectionListResponse, DeletionReportResponse};

    struct TestDir(std::path::PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Minimal `SharedState` for constructing the production router in tests.
    fn test_state(name: &str) -> (TestDir, SharedState) {
        let unique = format!(
            "verbatim-daemon-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let data_dir = TestDir(std::env::temp_dir().join(unique));
        std::fs::create_dir_all(&data_dir.0).unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, &data_dir.0).unwrap();
        let state = crate::tests::test_state(config, &data_dir.0, pipeline);
        (data_dir, state)
    }

    /// Materialize Axum path templates so oneshot requests hit the registered
    /// route (path params only need a non-empty segment).
    fn materialize_probe_path(template: &str) -> String {
        template
            .replace("{id}", "probe-id")
            .replace("{name}", "probe-name")
            .replace("{*eid}", "probe-eid")
            .replace("{eid}", "probe-eid")
            .replace("{*id}", "probe-id")
    }

    /// Probe with an unused method so a registered path yields 405 while an
    /// unregistered path yields routing-level 404 — without entering handlers
    /// that may themselves return application 404 for missing resources.
    async fn probe_registration(app: &Router, path: &str) -> StatusCode {
        let mut request = Request::builder()
            .method(Method::PATCH)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        // local-anonymous auth rejects non-loopback / missing ConnectInfo with
        // 403 before routing; loopback ConnectInfo lets the probe reach the
        // route table.
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
        ));
        app.clone().oneshot(request).await.unwrap().status()
    }

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
            ROUTE_REGISTRATION_COUNT, 40,
            "expected 40 daemon route registrations after report-artifact lookup"
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
            Some(PATH_REPORT_ARTIFACT_BY_ID)
        );
        // Spot-check registrations use the shared path-constant values (same
        // literals build_router passes to `.route(...)`).
        assert_eq!(registered_route_path_templates()[18], PATH_INGEST);
        assert_eq!(registered_route_path_templates()[32], PATH_TASK_BY_ID);
        assert_eq!(
            registered_route_path_templates(),
            [
                PATH_HEALTH,
                PATH_CONFIG,
                PATH_SOURCES,
                PATH_SOURCES,
                PATH_SOURCE_BY_ID,
                PATH_SOURCE_BY_ID,
                PATH_SOURCE_RELOCATIONS,
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
                PATH_REPORT_ARTIFACT_BY_ID,
            ]
        );
    }

    /// Couples [`ROUTE_PATH_TEMPLATES`] to the live Router from [`build_router`]:
    /// each inventory path must match a registration (status is not 404).
    ///
    /// Uses PATCH so path-matched routes return 405 without invoking handlers.
    /// Unregistered inventory entries get routing-level 404 and fail. A control
    /// path that is deliberately absent must 404, proving the probe can detect
    /// missing registrations. GET would also work for many routes (405 on
    /// POST-only paths; 401 on auth-gated paths) but can collide with
    /// handler-level resource 404s on path-param GET routes.
    #[tokio::test]
    async fn route_inventory_paths_are_registered_in_actual_router() {
        let (_test_dir, state) = test_state("route-inventory-router");
        let app = build_router(state);

        let control_status = probe_registration(&app, "/api/__not_registered_by_inventory__").await;
        assert_eq!(
            control_status,
            StatusCode::NOT_FOUND,
            "control path must be unregistered so the probe can detect inventory drift"
        );

        let mut probed = BTreeSet::new();
        for template in ROUTE_PATH_TEMPLATES {
            let path = materialize_probe_path(template);
            if !probed.insert(path.clone()) {
                // Duplicate inventory slots share one physical path (e.g. GET+POST
                // on `/api/collections`); one successful probe covers the path.
                continue;
            }
            let status = probe_registration(&app, &path).await;
            // 405 Method Not Allowed proves the path is registered but PATCH is
            // not accepted. 401/403 would also prove registration if auth blocked
            // before method matching. Only 404 means the path is missing from
            // the constructed Router.
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "inventory path template {template} (probe {path}) must be registered on the constructed Router; got {status}"
            );
        }
    }

    #[tokio::test]
    async fn collection_list_route_publishes_bound_response() {
        let (_test_dir, state) = test_state("collection-list-result-identity");
        let app = build_router(state);
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(PATH_COLLECTIONS)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
        ));

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(bytes.starts_with(b"{\"collections\":[],\"identity\":"));
        let response: CollectionListResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(response.collections.is_empty());
        assert_eq!(response.identity.kind.as_str(), "collection_list_result");
        assert_eq!(response.identity.schema_version.to_string(), "1.0.0");
        assert_eq!(response.identity.artifact_id, "collections");
    }

    #[tokio::test]
    async fn deletion_routes_bind_only_the_pending_receipt() {
        let unique = format!(
            "verbatim-daemon-deletion-result-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let data_dir = TestDir(std::env::temp_dir().join(unique));
        std::fs::create_dir_all(&data_dir.0).unwrap();
        let source_path = data_dir.0.join("pending.md");
        std::fs::write(&source_path, "delete through the production router").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.qdrant.enabled = true;
        config.qdrant.url = "http://127.0.0.1:9".into();
        let pipeline = IngestPipeline::new(&config, &data_dir.0).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let state = crate::tests::test_state(config, &data_dir.0, pipeline);
        let app = build_router(state);

        let mut delete_request = Request::builder()
            .method(Method::DELETE)
            .uri(PATH_SOURCE_BY_ID.replace("{id}", &source_id.0))
            .body(Body::empty())
            .unwrap();
        delete_request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
        ));
        let delete_response = app.clone().oneshot(delete_request).await.unwrap();
        assert_eq!(delete_response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(delete_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(bytes.starts_with(b"{\"source_id\":"));
        let response: DeletionReportResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.source_id, source_id);
        assert_eq!(response.identity.kind.as_str(), "deletion_report_result");
        assert_eq!(response.identity.schema_version.to_string(), "1.0.0");
        assert_eq!(response.identity.artifact_id, response.source_id.0);
        assert_eq!(
            response.report.status_for(DeletionProduct::Qdrant),
            Some(DeletionOutcome::Pending)
        );

        let mut list_request = Request::builder()
            .method(Method::GET)
            .uri(PATH_DELETION_REPORTS)
            .body(Body::empty())
            .unwrap();
        list_request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
        ));
        let list_response = app.oneshot(list_request).await.unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let bytes = to_bytes(list_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(bytes.starts_with(b"[{\"source_id\":"));
        assert!(!bytes
            .windows(b"\"identity\"".len())
            .any(|window| window == b"\"identity\""));
        let reports: Vec<PersistedDeletionReport> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reports.len(), 2);
        assert!(reports
            .iter()
            .all(|report| report.source_id == response.source_id));
        let latest = reports.last().unwrap();
        assert_eq!(latest.recorded_at, response.recorded_at);
        assert_eq!(latest.retention_policy, response.retention_policy);
        assert_eq!(latest.report, response.report);
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
        assert_eq!(ROUTE_PATH_TEMPLATES[9], PATH_COLLECTIONS);
        assert_eq!(ROUTE_PATH_TEMPLATES[10], PATH_COLLECTIONS);
        assert_eq!(ROUTE_PATH_TEMPLATES[11], PATH_COLLECTION_BY_NAME);
        assert_eq!(ROUTE_PATH_TEMPLATES[12], PATH_COLLECTION_BY_NAME);
        assert_eq!(ROUTE_PATH_TEMPLATES[13], PATH_COLLECTION_ROOTS);
        assert_eq!(ROUTE_PATH_TEMPLATES[14], PATH_COLLECTION_SYNC);
        assert_eq!(ROUTE_PATH_TEMPLATES[15], PATH_COLLECTION_STATUS);
        assert_eq!(ROUTE_PATH_TEMPLATES[16], PATH_COLLECTION_WATCHERS_STATUS);
        assert_eq!(ROUTE_PATH_TEMPLATES[17], PATH_COLLECTION_WATCHER);
    }
}
