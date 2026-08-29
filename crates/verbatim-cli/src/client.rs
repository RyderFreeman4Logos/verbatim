use std::fmt;
use std::io::{Read, Write};
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use verbatim_core::api::{
    AddCollectionRootRequest, AddCollectionRootResponse, AddSourceRequest, AddSourceResponse,
    ApiHttpMethod, AskRequest, CheckStaleResponse, CollectionApiEndpoint, CollectionResponse,
    CollectionStatusResponse, CollectionSyncRequest, CollectionSyncResponse,
    CollectionWatcherResponse, CollectionWatcherUpdateRequest, CollectionWatchersStatusResponse,
    ConfigResponse, CreateCollectionRequest, EvidenceResponse, HealthResponse, IndexGcRequest,
    IndexGcResponse, IndexProfileDeleteRequest, IndexProfileDeleteResponse, IndexStatusResponse,
    IngestResponse, ReindexRequest, ReindexResponse, RelocateSourceRequest, RetrieveRequest,
    RetrieveResponse, SourceResponse, TaskCreatedResponse, TaskEventsResponse, TaskIngestRequest,
    TaskListResponse, TaskMutationResponse, TaskProfileResponse, TaskSummaryResponse,
    VectorJsonCleanupRequest, VectorJsonCleanupResponse,
};
use verbatim_core::collection::CollectionRecord;
use verbatim_core::config::{self, Config, DaemonConfig};
use verbatim_core::graphrag::ReportArtifactManifest;

#[cfg(test)]
use crate::auth::HTTP_ERROR_TRUNCATION_MARKER;
use crate::{auth, render, sse};

const MAX_HTTP_ERROR_BODY_BYTES: usize = 4096;
const DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS: u64 = 300;
const DAEMON_HTTP_TIMEOUT_PADDING_SECONDS: u64 = 120;
const TASK_WAIT_TIMEOUT_EXIT_CODE: u8 = 124;

pub type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
pub enum CliError {
    Api(String),
    DaemonUnreachable(String),
    Io(std::io::Error),
    TaskWaitTimedOut { timeout: Duration },
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::DaemonUnreachable(_) => 2,
            Self::TaskWaitTimedOut { .. } => TASK_WAIT_TIMEOUT_EXIT_CODE,
            Self::Api(_) | Self::Io(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(message) | Self::DaemonUnreachable(message) => f.write_str(message),
            Self::Io(error) => write!(f, "{error}"),
            Self::TaskWaitTimedOut { timeout } => {
                write!(f, "task wait timed out after {}", format_duration(*timeout))
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Api(_) | Self::DaemonUnreachable(_) | Self::TaskWaitTimedOut { .. } => None,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn write_report_artifact<W: Write>(
    writer: &mut W,
    artifact: &ReportArtifactManifest,
) -> CliResult<()> {
    serde_json::to_writer_pretty(&mut *writer, artifact)
        .map_err(|error| CliError::Api(error.to_string()))?;
    writeln!(writer)?;
    Ok(())
}

pub trait DaemonClient {
    fn add_source(&self, path: &str) -> CliResult<AddSourceResponse>;
    fn list_sources(&self) -> CliResult<Vec<SourceResponse>>;
    fn get_source(&self, id: &str) -> CliResult<SourceResponse>;
    fn remove_source(&self, id: &str) -> CliResult<()>;
    fn relocate_source(&self, id: &str, new_path: &str) -> CliResult<SourceResponse>;
    fn check_sources(&self) -> CliResult<CheckStaleResponse>;
    fn create_collection(&self, request: &CreateCollectionRequest)
        -> CliResult<CollectionResponse>;
    fn add_collection_root(
        &self,
        name: &str,
        request: &AddCollectionRootRequest,
    ) -> CliResult<AddCollectionRootResponse>;
    fn list_collections(&self) -> CliResult<Vec<CollectionRecord>>;
    fn get_collection(&self, name: &str) -> CliResult<CollectionResponse>;
    fn delete_collection(&self, name: &str) -> CliResult<()>;
    fn sync_collection(
        &self,
        name: &str,
        request: &CollectionSyncRequest,
    ) -> CliResult<CollectionSyncResponse>;
    fn collection_status(&self, name: &str) -> CliResult<CollectionStatusResponse>;
    fn list_collection_watcher_statuses(&self) -> CliResult<CollectionWatchersStatusResponse>;
    fn collection_watcher_status(&self, name: &str) -> CliResult<CollectionWatcherResponse>;
    fn update_collection_watcher(
        &self,
        name: &str,
        request: &CollectionWatcherUpdateRequest,
    ) -> CliResult<CollectionWatcherResponse>;
    fn ingest(
        &self,
        source_id: Option<&str>,
        force: bool,
        embedding_profile_id: Option<&str>,
        vectors_only: bool,
    ) -> CliResult<IngestResponse>;
    fn reindex(&self, request: &ReindexRequest) -> CliResult<ReindexResponse>;
    fn submit_ask_task(&self, request: &AskRequest) -> CliResult<TaskCreatedResponse>;
    fn submit_ingest_task(
        &self,
        source_id: Option<&str>,
        force: bool,
        embedding_profile_id: Option<&str>,
        vectors_only: bool,
    ) -> CliResult<TaskCreatedResponse>;
    fn submit_reindex_task(&self, request: &ReindexRequest) -> CliResult<TaskCreatedResponse>;
    fn index_status(&self) -> CliResult<IndexStatusResponse>;
    fn index_gc(&self, request: &IndexGcRequest) -> CliResult<IndexGcResponse>;
    fn index_delete_profile(
        &self,
        request: &IndexProfileDeleteRequest,
    ) -> CliResult<IndexProfileDeleteResponse>;
    fn vector_json_cleanup(
        &self,
        request: &VectorJsonCleanupRequest,
    ) -> CliResult<VectorJsonCleanupResponse>;
    fn list_tasks(&self) -> CliResult<TaskListResponse>;
    fn get_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse>;
    fn get_task_profile(&self, task_id: &str) -> CliResult<TaskProfileResponse>;
    fn get_task_events(&self, task_id: &str, after: Option<i64>) -> CliResult<TaskEventsResponse>;
    fn wait_task<W>(
        &self,
        task_id: &str,
        after: Option<i64>,
        timeout: TaskWaitTimeout,
        stdout: &mut W,
    ) -> CliResult<()>
    where
        W: Write;
    fn cancel_task(&self, task_id: &str) -> CliResult<TaskMutationResponse>;
    fn resume_task(&self, task_id: &str) -> CliResult<TaskMutationResponse>;
    fn ask<W>(&self, request: &AskRequest, stdout: &mut W) -> CliResult<()>
    where
        W: Write;
    fn retrieve(&self, request: &RetrieveRequest) -> CliResult<RetrieveResponse>;
    fn get_evidence(&self, evidence_id: &str) -> CliResult<EvidenceResponse>;
    fn get_report_artifact(&self, artifact_id: &str) -> CliResult<ReportArtifactManifest> {
        let _ = artifact_id;
        Err(CliError::Api(
            "report-artifact lookup is not implemented".into(),
        ))
    }
    fn get_config(&self) -> CliResult<ConfigResponse>;
    fn health(&self) -> CliResult<HealthResponse>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskWaitTimeout {
    ConfigDefault,
    Bounded(Duration),
    Unbounded,
}

pub struct HttpDaemonClient {
    client: Client,
    base_url: Option<String>,
    auth_token: Option<String>,
}

impl HttpDaemonClient {
    pub fn new() -> Self {
        Self {
            client: auth::daemon_client(),
            base_url: None,
            auth_token: auth::daemon_auth_token(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: Some(base_url.into()),
            auth_token: None,
        }
    }

    fn base_url(&self) -> CliResult<String> {
        if let Some(base_url) = &self.base_url {
            return Ok(base_url.trim_end_matches('/').to_string());
        }

        let path = config::config_path();
        let bind = if path.exists() {
            Config::load_from(&path)
                .map_err(|error| {
                    CliError::Api(format!(
                        "failed to load config {}: {error:#}",
                        path.display()
                    ))
                })?
                .daemon
                .bind
        } else {
            DaemonConfig::default().bind
        };

        Ok(auth::bind_to_base_url(&bind))
    }

    fn url(&self, path: &str) -> CliResult<String> {
        Ok(format!("{}{}", self.base_url()?, path))
    }

    fn request_timeout(&self) -> CliResult<Duration> {
        let seconds = if self.base_url.is_some() {
            DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS
        } else {
            let path = config::config_path();
            if path.exists() {
                let config = Config::load_from(&path).map_err(|error| {
                    CliError::Api(format!(
                        "failed to load config {}: {error:#}",
                        path.display()
                    ))
                })?;
                daemon_http_timeout_seconds(&config)
            } else {
                DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS
            }
        };
        Ok(Duration::from_secs(seconds.max(1)))
    }

    fn task_wait_timeout(&self, timeout: TaskWaitTimeout) -> CliResult<Option<Duration>> {
        match timeout {
            TaskWaitTimeout::Bounded(duration) => Ok(Some(nonzero_duration(duration))),
            TaskWaitTimeout::Unbounded => Ok(None),
            TaskWaitTimeout::ConfigDefault => {
                let config = self.task_wait_config()?;
                Ok(resolve_task_wait_timeout(&config, timeout))
            }
        }
    }

    fn task_wait_config(&self) -> CliResult<Config> {
        if self.base_url.is_some() {
            return Ok(Config::default());
        }

        let path = config::config_path();
        if path.exists() {
            Config::load_from(&path).map_err(|error| {
                CliError::Api(format!(
                    "failed to load config {}: {error:#}",
                    path.display()
                ))
            })
        } else {
            Ok(Config::default())
        }
    }

    fn request_timeout_for_policy(
        &self,
        policy: RequestTimeoutPolicy,
    ) -> CliResult<Option<Duration>> {
        match policy {
            RequestTimeoutPolicy::Finite => Ok(Some(self.request_timeout()?)),
            RequestTimeoutPolicy::LongRunning => Ok(None),
        }
    }

    fn apply_timeout(
        &self,
        request: RequestBuilder,
        policy: RequestTimeoutPolicy,
    ) -> CliResult<RequestBuilder> {
        Ok(match self.request_timeout_for_policy(policy)? {
            Some(timeout) => request.timeout(timeout),
            None => request,
        })
    }

    fn request_json<T, B>(&self, method: Method, path: &str, body: Option<&B>) -> CliResult<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = self.url(path)?;
        let policy = json_timeout_policy(&method, path);
        let mut request = self.apply_timeout(
            auth::authorize_request(
                self.client.request(method, &url),
                &url,
                self.auth_token.as_deref(),
            ),
            policy,
        )?;
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(|error| request_error(&url, error))?;
        decode_response(response)
    }
}

impl Default for HttpDaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonClient for HttpDaemonClient {
    fn add_source(&self, path: &str) -> CliResult<AddSourceResponse> {
        self.request_json(
            Method::POST,
            "/api/sources",
            Some(&AddSourceRequest {
                path: path.to_string(),
            }),
        )
    }

    fn list_sources(&self) -> CliResult<Vec<SourceResponse>> {
        self.request_json::<Vec<SourceResponse>, ()>(Method::GET, "/api/sources", None)
    }

    fn get_source(&self, id: &str) -> CliResult<SourceResponse> {
        self.request_json::<SourceResponse, ()>(Method::GET, &format!("/api/sources/{id}"), None)
    }

    fn remove_source(&self, id: &str) -> CliResult<()> {
        let url = self.url(&format!("/api/sources/{id}"))?;
        let response = self
            .apply_timeout(
                auth::authorize_request(self.client.delete(&url), &url, self.auth_token.as_deref()),
                RequestTimeoutPolicy::LongRunning,
            )?
            .send()
            .map_err(|error| request_error(&url, error))?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(());
        }
        decode_response::<Value>(response).map(|_| ())
    }

    fn relocate_source(&self, id: &str, new_path: &str) -> CliResult<SourceResponse> {
        self.request_json(
            Method::POST,
            "/api/source-relocations",
            Some(&RelocateSourceRequest {
                source_id: id.to_string(),
                new_path: new_path.to_string(),
            }),
        )
    }

    fn check_sources(&self) -> CliResult<CheckStaleResponse> {
        self.request_json::<CheckStaleResponse, ()>(Method::POST, "/api/sources/check", None)
    }

    fn create_collection(
        &self,
        request: &CreateCollectionRequest,
    ) -> CliResult<CollectionResponse> {
        let route = CollectionApiEndpoint::CreateCollection;
        self.request_json(
            collection_method(route),
            route.path_template(),
            Some(request),
        )
    }

    fn add_collection_root(
        &self,
        name: &str,
        request: &AddCollectionRootRequest,
    ) -> CliResult<AddCollectionRootResponse> {
        let route = CollectionApiEndpoint::AddCollectionRoot;
        let response: AddCollectionRootResponse =
            self.request_json(collection_method(route), &route.path(name), Some(request))?;
        response
            .validate_for_collection(name)
            .map_err(|error| CliError::Api(error.to_string()))?;
        Ok(response)
    }

    fn list_collections(&self) -> CliResult<Vec<CollectionRecord>> {
        let route = CollectionApiEndpoint::ListCollections;
        self.request_json::<Vec<CollectionRecord>, ()>(
            collection_method(route),
            route.path_template(),
            None,
        )
    }

    fn get_collection(&self, name: &str) -> CliResult<CollectionResponse> {
        let route = CollectionApiEndpoint::GetCollection;
        self.request_json::<CollectionResponse, ()>(
            collection_method(route),
            &route.path(name),
            None,
        )
    }

    fn delete_collection(&self, name: &str) -> CliResult<()> {
        let route = CollectionApiEndpoint::DeleteCollection;
        let url = self.url(&route.path(name))?;
        let response = self
            .apply_timeout(
                auth::authorize_request(
                    self.client.request(collection_method(route), &url),
                    &url,
                    self.auth_token.as_deref(),
                ),
                RequestTimeoutPolicy::LongRunning,
            )?
            .send()
            .map_err(|error| request_error(&url, error))?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(());
        }
        decode_response::<Value>(response).map(|_| ())
    }

    fn sync_collection(
        &self,
        name: &str,
        request: &CollectionSyncRequest,
    ) -> CliResult<CollectionSyncResponse> {
        let route = CollectionApiEndpoint::SyncCollection;
        let response: CollectionSyncResponse =
            self.request_json(collection_method(route), &route.path(name), Some(request))?;
        response
            .validate_for_collection(name)
            .map_err(|error| CliError::Api(error.to_string()))?;
        Ok(response)
    }

    fn collection_status(&self, name: &str) -> CliResult<CollectionStatusResponse> {
        let route = CollectionApiEndpoint::CollectionStatus;
        let response: CollectionStatusResponse = self
            .request_json::<CollectionStatusResponse, ()>(
                collection_method(route),
                &route.path(name),
                None,
            )?;
        response
            .validate_for_collection(name)
            .map_err(|error| CliError::Api(error.to_string()))?;
        Ok(response)
    }

    fn list_collection_watcher_statuses(&self) -> CliResult<CollectionWatchersStatusResponse> {
        let route = CollectionApiEndpoint::ListCollectionWatcherStatuses;
        self.request_json::<CollectionWatchersStatusResponse, ()>(
            collection_method(route),
            route.path_template(),
            None,
        )
    }

    fn collection_watcher_status(&self, name: &str) -> CliResult<CollectionWatcherResponse> {
        let route = CollectionApiEndpoint::CollectionWatcherStatus;
        let response: CollectionWatcherResponse = self
            .request_json::<CollectionWatcherResponse, ()>(
                collection_method(route),
                &route.path(name),
                None,
            )?;
        response
            .validate_for_collection(name)
            .map_err(|error| CliError::Api(error.to_string()))?;
        Ok(response)
    }

    fn update_collection_watcher(
        &self,
        name: &str,
        request: &CollectionWatcherUpdateRequest,
    ) -> CliResult<CollectionWatcherResponse> {
        let route = CollectionApiEndpoint::UpdateCollectionWatcher;
        let response: CollectionWatcherResponse =
            self.request_json(collection_method(route), &route.path(name), Some(request))?;
        response
            .validate_for_collection(name)
            .map_err(|error| CliError::Api(error.to_string()))?;
        Ok(response)
    }

    fn ingest(
        &self,
        source_id: Option<&str>,
        force: bool,
        embedding_profile_id: Option<&str>,
        vectors_only: bool,
    ) -> CliResult<IngestResponse> {
        let path = match (source_id, force) {
            (Some(_), true) => {
                return Err(CliError::Api(
                    "--force is only supported for all-source ingest".into(),
                ));
            }
            (Some(id), false) => format!("/api/ingest/{id}"),
            (None, true) => "/api/ingest".into(),
            (None, false) => "/api/ingest".into(),
        };
        if force && vectors_only {
            return Err(CliError::Api(
                "--force is not supported with --vectors-only".into(),
            ));
        }
        if embedding_profile_id.is_some() && !vectors_only {
            return Err(CliError::Api(
                "--embedding-profile requires --vectors-only".into(),
            ));
        }
        let path = ingest_path_with_query(&path, force, embedding_profile_id, vectors_only);
        self.request_json::<IngestResponse, ()>(Method::POST, &path, None)
    }

    fn submit_ask_task(&self, request: &AskRequest) -> CliResult<TaskCreatedResponse> {
        self.request_json(Method::POST, "/api/tasks/ask", Some(request))
    }

    fn reindex(&self, request: &ReindexRequest) -> CliResult<ReindexResponse> {
        self.request_json(Method::POST, "/api/reindex", Some(request))
    }

    fn submit_ingest_task(
        &self,
        source_id: Option<&str>,
        force: bool,
        embedding_profile_id: Option<&str>,
        vectors_only: bool,
    ) -> CliResult<TaskCreatedResponse> {
        if source_id.is_some() && force {
            return Err(CliError::Api(
                "--force is only supported for all-source ingest".into(),
            ));
        }
        if force && vectors_only {
            return Err(CliError::Api(
                "--force is not supported with --vectors-only".into(),
            ));
        }
        if embedding_profile_id.is_some() && !vectors_only {
            return Err(CliError::Api(
                "--embedding-profile requires --vectors-only".into(),
            ));
        }
        let request = TaskIngestRequest {
            source_id: source_id.map(str::to_string),
            force,
            embedding_profile_id: embedding_profile_id.map(str::to_string),
            vectors_only,
        };
        self.request_json(Method::POST, "/api/tasks/ingest", Some(&request))
    }

    fn submit_reindex_task(&self, request: &ReindexRequest) -> CliResult<TaskCreatedResponse> {
        self.request_json(Method::POST, "/api/tasks/reindex", Some(request))
    }

    fn index_status(&self) -> CliResult<IndexStatusResponse> {
        self.request_json::<IndexStatusResponse, ()>(Method::GET, "/api/index/status", None)
    }

    fn index_gc(&self, request: &IndexGcRequest) -> CliResult<IndexGcResponse> {
        self.request_json(Method::POST, "/api/index/gc", Some(request))
    }

    fn index_delete_profile(
        &self,
        request: &IndexProfileDeleteRequest,
    ) -> CliResult<IndexProfileDeleteResponse> {
        self.request_json(Method::POST, "/api/index/profiles/delete", Some(request))
    }

    fn vector_json_cleanup(
        &self,
        request: &VectorJsonCleanupRequest,
    ) -> CliResult<VectorJsonCleanupResponse> {
        self.request_json(
            Method::POST,
            "/api/index/vector-json/cleanup",
            Some(request),
        )
    }

    fn list_tasks(&self) -> CliResult<TaskListResponse> {
        self.request_json::<TaskListResponse, ()>(
            Method::GET,
            "/api/tasks?status=active&limit=20",
            None,
        )
    }

    fn get_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse> {
        self.request_json::<TaskSummaryResponse, ()>(
            Method::GET,
            &format!("/api/tasks/{task_id}"),
            None,
        )
    }

    fn get_task_profile(&self, task_id: &str) -> CliResult<TaskProfileResponse> {
        self.request_json::<TaskProfileResponse, ()>(
            Method::GET,
            &format!("/api/tasks/{task_id}/profile"),
            None,
        )
    }

    fn get_task_events(&self, task_id: &str, after: Option<i64>) -> CliResult<TaskEventsResponse> {
        let path = match after {
            Some(after) => format!("/api/tasks/{task_id}/events?after={after}"),
            None => format!("/api/tasks/{task_id}/events"),
        };
        self.request_json::<TaskEventsResponse, ()>(Method::GET, &path, None)
    }

    fn wait_task<W>(
        &self,
        task_id: &str,
        after: Option<i64>,
        timeout: TaskWaitTimeout,
        stdout: &mut W,
    ) -> CliResult<()>
    where
        W: Write,
    {
        let path = match after {
            Some(after) => format!("/api/tasks/{task_id}/wait?after={after}"),
            None => format!("/api/tasks/{task_id}/wait"),
        };
        let url = self.url(&path)?;
        let effective_timeout = self.task_wait_timeout(timeout)?;
        let mut request =
            auth::authorize_request(self.client.get(&url), &url, self.auth_token.as_deref());
        if let Some(timeout) = effective_timeout {
            request = request.timeout(timeout);
        }
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                let error = wait_request_error(&url, error, effective_timeout);
                if matches!(&error, CliError::TaskWaitTimedOut { .. }) {
                    render::write_task_wait_timeout_summary(stdout, None)?;
                }
                return Err(error);
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(status, response));
        }
        match sse::consume_task_sse(response, stdout) {
            Ok(report) => {
                let _last_event = report.last_event;
                Ok(())
            }
            Err(error) => {
                let (source, last_event) = error.into_parts();
                if let Some(timeout) = effective_timeout.filter(|_| is_read_timeout_error(&source))
                {
                    render::write_task_wait_timeout_summary(stdout, last_event.as_ref())?;
                    return Err(CliError::TaskWaitTimedOut { timeout });
                }
                Err(source)
            }
        }
    }

    fn cancel_task(&self, task_id: &str) -> CliResult<TaskMutationResponse> {
        self.request_json::<TaskMutationResponse, ()>(
            Method::POST,
            &format!("/api/tasks/{task_id}/cancel"),
            None,
        )
    }

    fn resume_task(&self, task_id: &str) -> CliResult<TaskMutationResponse> {
        self.request_json::<TaskMutationResponse, ()>(
            Method::POST,
            &format!("/api/tasks/{task_id}/resume"),
            None,
        )
    }

    fn ask<W>(&self, request: &AskRequest, stdout: &mut W) -> CliResult<()>
    where
        W: Write,
    {
        let url = self.url("/api/ask/stream")?;
        let response = self
            .apply_timeout(
                auth::authorize_request(self.client.post(&url), &url, self.auth_token.as_deref()),
                RequestTimeoutPolicy::Finite,
            )?
            .json(request)
            .send()
            .map_err(|error| request_error(&url, error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_error(status, response));
        }
        sse::consume_ask_sse(response, stdout)
    }

    fn retrieve(&self, request: &RetrieveRequest) -> CliResult<RetrieveResponse> {
        self.request_json(Method::POST, "/api/retrieve", Some(request))
    }

    fn get_evidence(&self, evidence_id: &str) -> CliResult<EvidenceResponse> {
        self.request_json::<EvidenceResponse, ()>(
            Method::GET,
            &format!("/api/evidence/{}", encode_query_component(evidence_id)),
            None,
        )
    }

    fn get_report_artifact(&self, artifact_id: &str) -> CliResult<ReportArtifactManifest> {
        self.request_json::<ReportArtifactManifest, ()>(
            Method::GET,
            &format!(
                "/api/report-artifact/{}",
                encode_query_component(artifact_id)
            ),
            None,
        )
    }

    fn get_config(&self) -> CliResult<ConfigResponse> {
        self.request_json::<ConfigResponse, ()>(Method::GET, "/api/config", None)
    }

    fn health(&self) -> CliResult<HealthResponse> {
        self.request_json::<HealthResponse, ()>(Method::GET, "/api/health", None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestTimeoutPolicy {
    Finite,
    LongRunning,
}

fn json_timeout_policy(method: &Method, path: &str) -> RequestTimeoutPolicy {
    if is_long_running_mutation(method, path) {
        RequestTimeoutPolicy::LongRunning
    } else {
        RequestTimeoutPolicy::Finite
    }
}

fn is_long_running_mutation(method: &Method, path: &str) -> bool {
    if method == Method::POST {
        return path == "/api/sources"
            || path == "/api/sources/check"
            || path == "/api/source-relocations"
            || path == "/api/ingest"
            || path.starts_with("/api/ingest?")
            || path.starts_with("/api/ingest/")
            || path == "/api/reindex"
            || path == "/api/index/gc"
            || path == "/api/index/vector-json/cleanup"
            || path == "/api/tasks/reindex"
            || (path.starts_with("/api/collections/") && path.ends_with("/sync"));
    }
    false
}

fn collection_method(route: CollectionApiEndpoint) -> Method {
    match route.method() {
        ApiHttpMethod::Delete => Method::DELETE,
        ApiHttpMethod::Get => Method::GET,
        ApiHttpMethod::Post => Method::POST,
        ApiHttpMethod::Put => Method::PUT,
    }
}

fn daemon_http_timeout_seconds(config: &Config) -> u64 {
    let max_model_timeout = [
        config
            .embedding
            .enabled
            .then_some(config.embedding.timeout_seconds),
        config
            .rerank
            .enabled
            .then_some(config.rerank.timeout_seconds),
        config.chat.enabled.then_some(config.chat.timeout_seconds),
        config
            .vision
            .enabled
            .then_some(config.vision.timeout_seconds),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS);

    max_model_timeout
        .max(DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS)
        .saturating_add(DAEMON_HTTP_TIMEOUT_PADDING_SECONDS)
}

fn resolve_task_wait_timeout(config: &Config, timeout: TaskWaitTimeout) -> Option<Duration> {
    match timeout {
        TaskWaitTimeout::Bounded(duration) => Some(nonzero_duration(duration)),
        TaskWaitTimeout::Unbounded => None,
        TaskWaitTimeout::ConfigDefault => Some(Duration::from_secs(
            config.cli.task_wait_timeout_seconds.max(1),
        )),
    }
}

fn nonzero_duration(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_secs(1)
    } else {
        duration
    }
}

fn ingest_path_with_query(
    base_path: &str,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
) -> String {
    let mut params = Vec::new();
    if force {
        params.push("force=true".to_string());
    }
    if let Some(profile_id) = embedding_profile_id {
        params.push(format!(
            "embedding_profile_id={}",
            encode_query_component(profile_id)
        ));
    }
    if vectors_only {
        params.push("vectors_only=true".to_string());
    }
    if params.is_empty() {
        base_path.to_string()
    } else {
        format!("{base_path}?{}", params.join("&"))
    }
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn decode_response<T>(response: reqwest::blocking::Response) -> CliResult<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        return Err(http_error(status, response));
    }
    response
        .json::<T>()
        .map_err(|error| CliError::Api(format!("daemon returned invalid JSON: {error}")))
}

fn http_error(status: StatusCode, mut response: reqwest::blocking::Response) -> CliError {
    let (body, truncated) = read_bounded_error_body(&mut response)
        .unwrap_or_else(|error| (format!("<failed to read response body: {error}>"), false));
    if status.is_client_error() {
        if let Some(message) = daemon_error_message(&body) {
            return CliError::Api(message);
        }
    }
    CliError::Api(format!(
        "daemon returned HTTP {status}: {}",
        bounded_redacted_body(&body, truncated)
    ))
}

fn daemon_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let message = value.get("error")?.as_str()?.trim();
    (!message.is_empty()).then(|| auth::redact_text_secrets(message))
}

fn request_error(url: &str, error: reqwest::Error) -> CliError {
    if error.is_connect() || error.is_timeout() {
        return CliError::DaemonUnreachable(format!(
            "could not reach verbatim daemon at {url}: {error}\n\
             Start it with: systemctl --user start verbatim\n\
             If the address is wrong, check [daemon] bind in the config."
        ));
    }
    CliError::Api(format!("failed to call daemon at {url}: {error}"))
}

fn wait_request_error(url: &str, error: reqwest::Error, timeout: Option<Duration>) -> CliError {
    if error.is_timeout() {
        if let Some(timeout) = timeout {
            return CliError::TaskWaitTimedOut { timeout };
        }
    }
    request_error(url, error)
}

fn is_read_timeout_error(error: &CliError) -> bool {
    match error {
        CliError::Io(error) => {
            let display = error.to_string().to_ascii_lowercase();
            let debug = format!("{error:?}").to_ascii_lowercase();
            error.kind() == std::io::ErrorKind::TimedOut
                || display.contains("timed out")
                || display.contains("timeout")
                || display.contains("deadline")
                || debug.contains("timedout")
                || debug.contains("timed out")
                || debug.contains("timeout")
                || debug.contains("deadline")
        }
        CliError::Api(_) | CliError::DaemonUnreachable(_) | CliError::TaskWaitTimedOut { .. } => {
            false
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        format!("{}ms", duration.as_millis().max(1))
    } else if seconds.is_multiple_of(86_400) {
        format!("{}d", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("{}h", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn read_bounded_error_body<R>(reader: &mut R) -> std::io::Result<(String, bool)>
where
    R: Read,
{
    let mut bytes = Vec::with_capacity(MAX_HTTP_ERROR_BODY_BYTES + 1);
    let mut limited = reader.take((MAX_HTTP_ERROR_BODY_BYTES + 1) as u64);
    limited.read_to_end(&mut bytes)?;
    let truncated = bytes.len() > MAX_HTTP_ERROR_BODY_BYTES;
    if truncated {
        bytes.truncate(MAX_HTTP_ERROR_BODY_BYTES);
    }

    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn bounded_redacted_body(body: &str, truncated: bool) -> String {
    auth::redact_response_body(body, truncated)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    include!("tests/issue_332_client_route_tests.rs");
    include!("tests/collection_sync_http_fixture.rs");
    include!("tests/report_artifact_evidence_route_tests.rs");
    include!("tests/report_artifact_lookup_route_tests.rs");
    include!("tests/task_wait_client_fixtures.rs");

    use verbatim_core::api::TaskWaitEvent;
    use verbatim_core::task::{TaskEvent, TaskSpan, TaskSummary};

    #[test]
    fn bind_to_base_url_adds_http_scheme() {
        assert_eq!(
            auth::bind_to_base_url("127.0.0.1:7700"),
            "http://127.0.0.1:7700"
        );
        assert_eq!(
            auth::bind_to_base_url("http://127.0.0.1:7700/"),
            "http://127.0.0.1:7700"
        );
    }

    #[test]
    fn daemon_http_timeout_uses_largest_enabled_model_timeout_with_padding() {
        let mut config = default_config();
        config.embedding.enabled = true;
        config.embedding.timeout_seconds = 1800;
        config.rerank.timeout_seconds = 90;
        config.chat.timeout_seconds = 600;
        config.vision.timeout_seconds = 45;

        assert_eq!(daemon_http_timeout_seconds(&config), 1920);
    }

    #[test]
    fn daemon_http_timeout_ignores_disabled_model_timeouts() {
        let mut config = default_config();
        config.embedding.enabled = false;
        config.embedding.timeout_seconds = 7200;
        config.rerank.enabled = false;
        config.chat.enabled = true;
        config.chat.timeout_seconds = 300;
        config.vision.enabled = false;

        assert_eq!(daemon_http_timeout_seconds(&config), 420);
    }

    #[test]
    fn long_running_mutations_do_not_use_finite_request_timeout() {
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/sources"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/sources/check"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/source-relocations"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/ingest"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/ingest?force=true"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/ingest/src-1"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/reindex"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/index/gc"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/index/vector-json/cleanup"),
            RequestTimeoutPolicy::LongRunning
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/tasks/reindex"),
            RequestTimeoutPolicy::LongRunning
        );
    }

    #[test]
    fn interactive_and_read_only_requests_keep_finite_request_timeout() {
        assert_eq!(
            json_timeout_policy(&Method::GET, "/api/sources"),
            RequestTimeoutPolicy::Finite
        );
        assert_eq!(
            json_timeout_policy(&Method::GET, "/api/config"),
            RequestTimeoutPolicy::Finite
        );
        assert_eq!(
            json_timeout_policy(&Method::POST, "/api/ask/stream"),
            RequestTimeoutPolicy::Finite
        );
    }

    #[test]
    fn long_running_timeout_policy_is_unbounded() {
        let client = HttpDaemonClient::with_base_url("http://127.0.0.1:1");

        assert_eq!(
            client
                .request_timeout_for_policy(RequestTimeoutPolicy::LongRunning)
                .unwrap(),
            None
        );
        assert_eq!(
            client
                .request_timeout_for_policy(RequestTimeoutPolicy::Finite)
                .unwrap(),
            Some(Duration::from_secs(DEFAULT_DAEMON_HTTP_TIMEOUT_SECONDS))
        );
    }

    #[test]
    fn task_wait_timeout_resolution_uses_cli_config_and_overrides() {
        let mut config = default_config();
        config.cli.task_wait_timeout_seconds = 25;

        assert_eq!(
            resolve_task_wait_timeout(&config, TaskWaitTimeout::ConfigDefault),
            Some(Duration::from_secs(25))
        );
        assert_eq!(
            resolve_task_wait_timeout(&config, TaskWaitTimeout::Bounded(Duration::from_secs(1500))),
            Some(Duration::from_secs(1500))
        );
        assert_eq!(
            resolve_task_wait_timeout(&config, TaskWaitTimeout::Unbounded),
            None
        );
    }

    #[test]
    fn task_wait_timeout_error_uses_distinct_exit_code() {
        let error = CliError::TaskWaitTimedOut {
            timeout: Duration::from_secs(1500),
        };

        assert_eq!(error.exit_code(), TASK_WAIT_TIMEOUT_EXIT_CODE);
        assert_eq!(error.to_string(), "task wait timed out after 25m");
    }

    #[test]
    fn http_add_source_posts_json_to_daemon() {
        let server = TestServer::respond_once(ADD_SOURCE_RESPONSE);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.add_source("/tmp/doc.pdf").unwrap();

        assert_eq!(response.id, "src-1");
        let request = server.request();
        assert!(request.starts_with("POST /api/sources HTTP/1.1"));
        assert!(request.contains("\"path\":\"/tmp/doc.pdf\""));
    }

    include!("tests/collection_watcher_http_fixture.rs");

    #[test]
    fn http_ingest_force_uses_query_parameter() {
        let server = TestServer::respond_many([ingest_http_response(2)]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.ingest(None, true, None, false).unwrap();

        assert_eq!(response.ingested, 2);
        assert!(server
            .request()
            .starts_with("POST /api/ingest?force=true HTTP/1.1"));
    }

    #[test]
    fn http_ingest_profile_vectors_only_uses_query_parameters() {
        let server = TestServer::respond_many([ingest_http_response(1)]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client
            .ingest(Some("src-1"), false, Some("alt.profile"), true)
            .unwrap();

        assert_eq!(response.ingested, 1);
        assert!(server.request().starts_with(
            "POST /api/ingest/src-1?embedding_profile_id=alt.profile&vectors_only=true HTTP/1.1"
        ));
    }

    fn ingest_http_response(ingested: usize) -> String {
        let body =
            serde_json::to_string(&IngestResponse::new(ingested).expect("ingest response fixture"))
                .expect("ingest response encodes");
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
        )
    }

    fn task_created_http_response(task_id: &str) -> String {
        let body = serde_json::to_string(
            &TaskCreatedResponse::new(task_id).expect("task-created response fixture"),
        )
        .expect("task-created response encodes");
        format!(
            "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
        )
    }

    #[test]
    fn http_reindex_posts_json_to_daemon() {
        let server = TestServer::respond_many(vec![
            reindex_http_response(1),
            task_created_http_response("task-1"),
            reindex_http_response(2),
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());
        let request = ReindexRequest {
            source_id: Some("src-1".into()),
            all: false,
            stale: false,
            force: false,
            embedding_profile_id: Some("alt.profile".into()),
            vectors_only: true,
        };

        let response = client.reindex(&request).unwrap();
        let task = client.submit_reindex_task(&request).unwrap();
        let force_response = client
            .reindex(&ReindexRequest {
                source_id: None,
                all: false,
                stale: false,
                force: true,
                embedding_profile_id: None,
                vectors_only: false,
            })
            .unwrap();

        assert_eq!(response.reindexed, 1);
        assert_eq!(task.task_id, "task-1");
        assert_eq!(force_response.reindexed, 2);
        let requests = server.requests();
        assert!(requests[0].starts_with("POST /api/reindex HTTP/1.1"));
        assert!(requests[0].contains("\"source_id\":\"src-1\""));
        assert!(requests[0].contains("\"embedding_profile_id\":\"alt.profile\""));
        assert!(requests[1].starts_with("POST /api/tasks/reindex HTTP/1.1"));
        assert!(requests[2].starts_with("POST /api/reindex HTTP/1.1"));
        assert!(requests[2].contains("\"force\":true"));
        assert!(requests[2].contains("\"all\":false"));
        assert!(!requests[2].contains("\"source_id\""));
    }

    fn reindex_http_response(reindexed: usize) -> String {
        let body = serde_json::to_string(
            &ReindexResponse::new(reindexed).expect("reindex response fixture"),
        )
        .expect("reindex response encodes");
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
        )
    }

    #[test]
    fn http_index_gc_posts_dry_run_request() {
        let body = serde_json::to_string(
            &IndexGcResponse::new(
                true,
                verbatim_core::index_gc::IndexGcConfig {
                    retain_previous_generations: 2,
                    stale_staging_seconds: 86_400,
                },
                verbatim_core::index_gc::IndexGcPlan {
                    entries: vec![verbatim_core::index_gc::IndexGcPlanEntry {
                        path: "/tmp/verbatim/indexes/profiles/default/gen-1".into(),
                        kind: verbatim_core::index_gc::IndexGcArtifactKind::Generation,
                        profile_id: Some("default".into()),
                        generation: Some(1),
                        approximate_bytes: 10,
                        reason: "old".into(),
                    }],
                    skipped: vec![],
                    approximate_reclaim_bytes: 10,
                },
                verbatim_core::index_gc::IndexGcApplyReport::default(),
            )
            .expect("index GC response fixture"),
        )
        .expect("index GC response encodes");
        let server = TestServer::respond_many(vec![json_response("200 OK", &body)]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.index_gc(&IndexGcRequest { dry_run: true }).unwrap();

        assert!(response.dry_run);
        assert_eq!(response.plan.entries.len(), 1);
        let request = server.request();
        assert!(request.starts_with("POST /api/index/gc HTTP/1.1"));
        assert!(request.contains("\"dry_run\":true"));
    }

    fn vector_json_cleanup_http_response(dry_run: bool) -> String {
        let response = VectorJsonCleanupResponse::new(
            dry_run,
            verbatim_core::store::VectorJsonCleanupReport {
                tables: verbatim_core::store::VectorJsonCleanupTables {
                    chunk_vectors: verbatim_core::store::VectorJsonCleanupTableStats {
                        eligible: 1,
                        already_clean: 2,
                        json_only: 3,
                        missing_blob: 4,
                        malformed_blob: 5,
                    },
                    embedding_cache: verbatim_core::store::VectorJsonCleanupTableStats {
                        eligible: 6,
                        already_clean: 7,
                        json_only: 8,
                        missing_blob: 9,
                        malformed_blob: 10,
                    },
                },
                cleared: Default::default(),
            },
        )
        .expect("vector JSON cleanup response fixture");
        json_response(
            "200 OK",
            &serde_json::to_string(&response).expect("cleanup response encodes"),
        )
    }

    #[test]
    fn http_vector_json_cleanup_posts_request() {
        let server = TestServer::respond_many(vec![vector_json_cleanup_http_response(true)]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client
            .vector_json_cleanup(&VectorJsonCleanupRequest {
                dry_run: true,
                confirm: false,
            })
            .unwrap();

        assert!(response.dry_run);
        assert_eq!(response.report.tables.chunk_vectors.eligible, 1);
        assert_eq!(response.report.tables.embedding_cache.malformed_blob, 10);
        let request = server.request();
        assert!(request.starts_with("POST /api/index/vector-json/cleanup HTTP/1.1"));
        assert!(request.contains("\"dry_run\":true"));
        assert!(request.contains("\"confirm\":false"));
    }

    #[test]
    fn http_task_routes_are_plumbed() {
        let task_summary = concat!(
            "{\"task\":{\"id\":\"task-1\",\"kind\":\"ask\",\"status\":\"succeeded\",",
            "\"created_at\":\"1\",\"updated_at\":\"2\",\"started_at\":\"1\",\"finished_at\":\"2\",",
            "\"request\":{\"question_chars\":4},\"result\":{\"citation_count\":1},\"error\":null},",
            "\"spans\":[{\"sequence\":1,\"task_id\":\"task-1\",\"phase\":\"chat\",",
            "\"started_at\":\"1\",\"duration_ms\":5,\"metadata\":{\"citation_count\":1}}],",
            "\"identity\":{\"kind\":\"task_run\",\"schema_version\":{\"major\":1,\"minor\":0,\"patch\":0},",
            "\"artifact_id\":\"task-1\",\"content_hash\":\"502537cc588ccc5d7edf2dd54f0fcc3ad2b5fecad0623eb56d185ddd1956f2ba\"}}"
        );
        let task_mutation = concat!(
            "{\"task\":{\"id\":\"task-1\",\"kind\":\"ask\",\"status\":\"succeeded\",",
            "\"created_at\":\"1\",\"updated_at\":\"2\",\"started_at\":\"1\",\"finished_at\":\"2\",",
            "\"request\":{\"question_chars\":4},\"result\":{\"citation_count\":1},\"error\":null},",
            "\"spans\":[{\"sequence\":1,\"task_id\":\"task-1\",\"phase\":\"chat\",",
            "\"started_at\":\"1\",\"duration_ms\":5,\"metadata\":{\"citation_count\":1}}],",
            "\"identity\":{\"kind\":\"task_run\",\"schema_version\":{\"major\":1,\"minor\":0,\"patch\":0},",
            "\"artifact_id\":\"task-1\",\"content_hash\":\"502537cc588ccc5d7edf2dd54f0fcc3ad2b5fecad0623eb56d185ddd1956f2ba\"}}"
        );
        let server = TestServer::respond_many(vec![
            task_created_http_response("task-1"),
            task_created_http_response("task-2"),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_summary}"
            ),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"task_id\":\"task-1\",\"events\":[{\"sequence\":2,\"task_id\":\"task-1\",\"event_type\":\"phase\",\"message\":\"done\",\"payload\":{},\"created_at\":\"2\"}],\"identity\":{\"kind\":\"task_events\",\"schema_version\":{\"major\":1,\"minor\":0,\"patch\":0},\"artifact_id\":\"task-1\",\"content_hash\":\"d3b255d8f296abc290572a161ad36ef1cd2e84370b299d6a77a08bc05609a3c6\"}}".to_string(),
            task_wait_response(
                TASK_WAIT_TERMINAL_TASK,
                "[]",
                "[]",
                true,
            ),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_mutation}"
            ),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_mutation}"
            ),
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());
        let ask = AskRequest {
            question: "Why?".into(),
            source_id: Some("src-1".into()),
            collection_filter: Default::default(),
            embedding_profile_id: None,
            show_retrieval: false,
            context_only: false,
            limit: None,
            page_size: None,
            page: None,
        };

        assert_eq!(client.submit_ask_task(&ask).unwrap().task_id, "task-1");
        assert_eq!(
            client
                .submit_ingest_task(Some("src-1"), false, None, false)
                .unwrap()
                .task_id,
            "task-2"
        );
        let summary = client.get_task("task-1").unwrap();
        assert_eq!(summary.task.id.0, "task-1");
        assert_eq!(summary.identity.artifact_id, "task-1");
        assert_eq!(
            client.get_task_events("task-1", Some(1)).unwrap().events[0].sequence,
            2
        );
        let mut stdout = Vec::new();
        client
            .wait_task("task-1", Some(2), TaskWaitTimeout::Unbounded, &mut stdout)
            .unwrap();
        assert!(String::from_utf8(stdout).unwrap().contains("Task: task-1"));
        let cancelled = client.cancel_task("task-1").unwrap();
        assert_eq!(cancelled.task.status.as_str(), "succeeded");
        assert_eq!(cancelled.identity.artifact_id, "task-1");
        let resumed = client.resume_task("task-1").unwrap();
        assert_eq!(resumed.task.status.as_str(), "succeeded");
        assert_eq!(resumed.identity.artifact_id, "task-1");

        let requests = server.requests();
        assert!(requests[0].starts_with("POST /api/tasks/ask HTTP/1.1"));
        assert!(requests[0].contains("\"question\":\"Why?\""));
        assert!(requests[1].starts_with("POST /api/tasks/ingest HTTP/1.1"));
        assert!(requests[1].contains("\"source_id\":\"src-1\""));
        assert!(requests[2].starts_with("GET /api/tasks/task-1 HTTP/1.1"));
        assert!(requests[3].starts_with("GET /api/tasks/task-1/events?after=1 HTTP/1.1"));
        assert!(requests[4].starts_with("GET /api/tasks/task-1/wait?after=2 HTTP/1.1"));
        assert!(requests[5].starts_with("POST /api/tasks/task-1/cancel HTTP/1.1"));
        assert!(requests[6].starts_with("POST /api/tasks/task-1/resume HTTP/1.1"));
    }

    #[test]
    fn http_task_profile_route_is_plumbed() {
        let server = TestServer::respond_many(vec![concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
            "{\"profile\":{\"schema_version\":1,\"task_id\":\"task-1\",",
            "\"task_kind\":\"retrieve\",\"status\":\"succeeded\",",
            "\"queue_wait_ms\":0,\"total_wall_ms\":12},\"identity\":{",
            "\"kind\":\"task_profile\",\"schema_version\":{\"major\":1,",
            "\"minor\":0,\"patch\":0},\"artifact_id\":\"task-1\",",
            "\"content_hash\":\"9a53920d70aa1d82e3c8d9a4ad19a5e0dad453318286ea53566e7664c95c506b\"}}"
        )
        .to_string()]);
        let profile = HttpDaemonClient::with_base_url(server.base_url())
            .get_task_profile("task-1")
            .unwrap()
            .profile;

        assert_eq!(profile.task_id.0, "task-1");
        assert!(server.requests()[0].starts_with("GET /api/tasks/task-1/profile HTTP/1.1"));
    }

    #[test]
    fn http_task_wait_send_timeout_returns_distinct_error_and_no_event_summary() {
        let server = TestServer::respond_delayed_response(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
            Duration::from_millis(250),
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());
        let mut stdout = Vec::new();

        let error = client
            .wait_task(
                "task-1",
                None,
                TaskWaitTimeout::Bounded(Duration::from_millis(50)),
                &mut stdout,
            )
            .unwrap_err();

        assert_eq!(error.exit_code(), TASK_WAIT_TIMEOUT_EXIT_CODE);
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Last known task state before timeout:"));
        assert!(output.contains("unavailable: no task event received"));
        assert!(server
            .request()
            .starts_with("GET /api/tasks/task-1/wait HTTP/1.1"));
    }

    #[test]
    fn http_task_wait_read_timeout_returns_distinct_error_and_last_state() {
        let wait_response = Box::leak(
            task_wait_response(TASK_WAIT_RUNNING_TASK, "[]", "[]", false).into_boxed_str(),
        );
        let server = TestServer::respond_slow_stream(wait_response, Duration::from_secs(2));
        let client = HttpDaemonClient::with_base_url(server.base_url());
        let mut stdout = Vec::new();

        let error = client
            .wait_task(
                "task-1",
                None,
                TaskWaitTimeout::Bounded(Duration::from_millis(500)),
                &mut stdout,
            )
            .unwrap_err();

        assert_eq!(error.exit_code(), TASK_WAIT_TIMEOUT_EXIT_CODE);
        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("Task task-1 status=running"));
        assert!(output.contains("progress: phase=chat elapsed=100ms"));
        assert!(output.contains("Last known task state before timeout:"));
        assert!(server
            .request()
            .starts_with("GET /api/tasks/task-1/wait HTTP/1.1"));
    }

    #[test]
    fn http_evidence_config_and_status_parse_json() {
        let server = TestServer::respond_many([
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"ev-1\",\"source_id\":\"src-1\",\"source_hash\":\"persisted-source-hash\",\"source_bounded\":true,\"text_hash\":\"receipt-text-hash\",\"kind\":\"text\",\"derived_from\":null,\"locator\":\"PDF p.1 para.1\",\"structured_locator\":{\"type\":\"Pdf\",\"page\":1,\"paragraph\":1,\"bbox\":null},\"text\":\"quoted\",\"heading_path\":[],\"position\":0,\"image_artifact\":null}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"config\":{\"daemon\":{\"bind\":\"x\"}},\"reload\":{\"active_config_path\":\"/tmp/config.toml\",\"loaded_at\":\"1\",\"last_applied_reload_safe_keys\":[],\"last_restart_required_keys\":[]}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}",
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let evidence = client.get_evidence("ev-1").unwrap();
        assert_eq!(evidence.id, "ev-1");
        assert_eq!(
            evidence.source_hash.as_deref(),
            Some("persisted-source-hash")
        );
        assert_eq!(client.get_config().unwrap().config["daemon"]["bind"], "x");
        assert_eq!(client.health().unwrap().status, "ok");

        let requests = server.requests();
        assert!(requests[0].starts_with("GET /api/evidence/ev-1 HTTP/1.1"));
        assert!(requests[1].starts_with("GET /api/config HTTP/1.1"));
        assert!(requests[2].starts_with("GET /api/health HTTP/1.1"));
    }

    #[test]
    fn http_error_reports_status_and_redacted_bounded_body() {
        let server = TestServer::respond_once(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"bad\",\"api_key\":\"should-not-print\"}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let error = client.health().unwrap_err();

        assert_eq!(error.exit_code(), 1);
        let message = error.to_string();
        assert!(message.contains("HTTP 500"));
        assert!(message.contains("<redacted>"));
        assert!(!message.contains("should-not-print"));
    }

    #[test]
    fn http_remove_missing_source_reports_concise_not_found() {
        let server = TestServer::respond_once(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"source not found: __missing_source_smoke_retest__\"}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let error = client
            .remove_source("__missing_source_smoke_retest__")
            .unwrap_err();

        assert_eq!(error.exit_code(), 1);
        let message = error.to_string();
        assert_eq!(message, "source not found: __missing_source_smoke_retest__");
        assert!(!message.contains("HTTP 500"));
        assert!(!message.contains("Internal Server Error"));
        assert!(server
            .request()
            .starts_with("DELETE /api/sources/__missing_source_smoke_retest__ HTTP/1.1"));
    }

    #[test]
    fn http_client_error_reports_daemon_error_without_json_wrapper() {
        let server = TestServer::respond_once(
            "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"task profile unavailable for incomplete task task-1 (status queued)\"}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let error = client.get_task_profile("task-1").unwrap_err();

        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            error.to_string(),
            "task profile unavailable for incomplete task task-1 (status queued)"
        );
        assert!(server
            .request()
            .starts_with("GET /api/tasks/task-1/profile HTTP/1.1"));
    }

    #[test]
    fn http_retrieve_starting_error_uses_daemon_json_message() {
        let server = TestServer::respond_once(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"verbatim daemon is starting; retrieval is not ready (startup_phase=orphan_recovery; degraded_reason=recovering previous running ingest tasks)\",\"code\":\"retrieval_not_ready\",\"readiness\":\"starting\",\"retrieval_ready\":false,\"startup_phase\":\"orphan_recovery\",\"degraded_reason\":\"recovering previous running ingest tasks\"}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let error = client
            .retrieve(&RetrieveRequest {
                question: "question".into(),
                source_id: None,
                collection_filter: Default::default(),
                embedding_profile_id: None,
                limit: None,
                page_size: None,
                page: None,
                fast: false,
                rerank: None,
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            })
            .unwrap_err();

        assert_eq!(error.exit_code(), 1);
        let message = error.to_string();
        assert!(message.contains("verbatim daemon is starting"));
        assert!(message.contains("retrieval is not ready"));
        assert!(message.contains("startup_phase=orphan_recovery"));
        assert!(!message.contains("could not reach"));
        assert!(server.request().starts_with("POST /api/retrieve HTTP/1.1"));
    }

    #[test]
    fn long_error_body_redacts_secret_before_truncation_boundary() {
        let body = format!(
            "{{\"api_key\":\"should-not-print\",\"padding\":\"{}\"",
            "x".repeat(MAX_HTTP_ERROR_BODY_BYTES * 2)
        );
        let (bounded, truncated) =
            read_bounded_error_body(&mut Cursor::new(body.as_bytes())).unwrap();

        let message = bounded_redacted_body(&bounded, truncated);

        assert!(truncated);
        assert!(message.contains("<redacted>"));
        assert!(message.contains(HTTP_ERROR_TRUNCATION_MARKER));
        assert!(!message.contains("should-not-print"));
        assert!(message.len() <= MAX_HTTP_ERROR_BODY_BYTES + HTTP_ERROR_TRUNCATION_MARKER.len());
    }

    #[test]
    fn long_error_body_does_not_read_secret_after_truncation_boundary() {
        let body = format!(
            "{{\"padding\":\"{}\",\"api_key\":\"after-boundary\"}}",
            "x".repeat(MAX_HTTP_ERROR_BODY_BYTES + 100)
        );
        let (bounded, truncated) =
            read_bounded_error_body(&mut Cursor::new(body.as_bytes())).unwrap();

        let message = bounded_redacted_body(&bounded, truncated);

        assert!(truncated);
        assert!(message.contains(HTTP_ERROR_TRUNCATION_MARKER));
        assert!(!message.contains("after-boundary"));
        assert!(message.len() <= MAX_HTTP_ERROR_BODY_BYTES + HTTP_ERROR_TRUNCATION_MARKER.len());
    }

    #[test]
    fn unreachable_daemon_uses_exit_code_two_with_hint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = HttpDaemonClient::with_base_url(base_url);

        let error = client.health().unwrap_err();

        assert_eq!(error.exit_code(), 2);
        let message = error.to_string();
        assert!(message.contains("systemctl --user start verbatim"));
        assert!(message.contains("[daemon] bind"));
    }

    struct TestServer {
        addr: std::net::SocketAddr,
        requests: Arc<Mutex<Vec<String>>>,
        handle: thread::JoinHandle<()>,
    }

    impl TestServer {
        fn respond_once(response: &'static str) -> Self {
            Self::respond_many([response])
        }

        fn respond_many<I, S>(responses: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            let responses = responses.into_iter().map(Into::into).collect::<Vec<_>>();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_http_request(&mut stream);
                    thread_requests.lock().unwrap().push(request);
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            Self {
                addr,
                requests,
                handle,
            }
        }

        fn respond_slow_stream(response: &'static str, hold_open: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
                thread::sleep(hold_open);
            });
            Self {
                addr,
                requests,
                handle,
            }
        }

        fn respond_delayed_response(response: &'static str, response_delay: Duration) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                thread::sleep(response_delay);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            });
            Self {
                addr,
                requests,
                handle,
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn request(self) -> String {
            let mut requests = self.requests();
            requests.remove(0)
        }

        fn requests(self) -> Vec<String> {
            self.handle.join().unwrap();
            self.requests.lock().unwrap().clone()
        }
    }

    fn default_config() -> Config {
        serde_json::from_value(serde_json::json!({})).expect("empty config uses serde defaults")
    }

    fn json_response(status: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}")
    }

    fn assert_collection_request(
        request: &str,
        route: CollectionApiEndpoint,
        collection_name: Option<&str>,
    ) {
        let path = collection_name.map_or_else(
            || route.path_template().to_string(),
            |name| route.path(name),
        );
        let expected = format!("{} {path} HTTP/1.1", route.method().as_str());
        assert!(
            request.starts_with(&expected),
            "expected request line {expected:?}, got {request:?}"
        );
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if request_complete(&buffer) {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn request_complete(buffer: &[u8]) -> bool {
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .or_else(|| {
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        buffer.len() >= header_end + 4 + content_length
    }
}
