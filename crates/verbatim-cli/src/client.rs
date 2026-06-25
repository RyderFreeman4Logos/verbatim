use std::fmt;
use std::io::{Read, Write};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::blocking::RequestBuilder;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use verbatim_core::api::{
    AddCollectionRootRequest, AddSourceRequest, AddSourceResponse, ApiHttpMethod, AskRequest,
    CheckStaleResponse, CollectionApiEndpoint, CollectionResponse, CollectionStatusResponse,
    CollectionSyncRequest, CollectionSyncResponse, CollectionWatcherResponse,
    CollectionWatcherUpdateRequest, CollectionWatchersStatusResponse, ConfigResponse,
    CreateCollectionRequest, EvidenceResponse, HealthResponse, IndexGcRequest, IndexGcResponse,
    IngestResponse, ReindexRequest, ReindexResponse, RetrieveRequest, RetrieveResponse,
    SourceResponse, TaskCreatedResponse, TaskEventsResponse, TaskIngestRequest,
    TaskSummaryResponse,
};
use verbatim_core::collection::CollectionRecord;
use verbatim_core::config::{self, Config, DaemonConfig};

use crate::{render, sse};

const MAX_HTTP_ERROR_BODY_BYTES: usize = 4096;
const HTTP_ERROR_TRUNCATION_MARKER: &str = "...[truncated]";
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

pub trait DaemonClient {
    fn add_source(&self, path: &str) -> CliResult<AddSourceResponse>;
    fn list_sources(&self) -> CliResult<Vec<SourceResponse>>;
    fn get_source(&self, id: &str) -> CliResult<SourceResponse>;
    fn remove_source(&self, id: &str) -> CliResult<()>;
    fn check_sources(&self) -> CliResult<CheckStaleResponse>;
    fn create_collection(&self, request: &CreateCollectionRequest)
        -> CliResult<CollectionResponse>;
    fn add_collection_root(
        &self,
        name: &str,
        request: &AddCollectionRootRequest,
    ) -> CliResult<CollectionResponse>;
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
    fn index_gc(&self, request: &IndexGcRequest) -> CliResult<IndexGcResponse>;
    fn get_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse>;
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
    fn cancel_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse>;
    fn resume_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse>;
    fn ask<W>(&self, request: &AskRequest, stdout: &mut W) -> CliResult<()>
    where
        W: Write;
    fn retrieve(&self, request: &RetrieveRequest) -> CliResult<RetrieveResponse>;
    fn get_evidence(&self, evidence_id: &str) -> CliResult<EvidenceResponse>;
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
}

impl HttpDaemonClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: None,
        }
    }

    #[cfg(test)]
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: Some(base_url.into()),
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

        Ok(bind_to_base_url(&bind))
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
        let mut request = self.apply_timeout(self.client.request(method, &url), policy)?;
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
            .apply_timeout(self.client.delete(&url), RequestTimeoutPolicy::LongRunning)?
            .send()
            .map_err(|error| request_error(&url, error))?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(());
        }
        decode_response::<Value>(response).map(|_| ())
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
    ) -> CliResult<CollectionResponse> {
        let route = CollectionApiEndpoint::AddCollectionRoot;
        self.request_json(collection_method(route), &route.path(name), Some(request))
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
                self.client.request(collection_method(route), &url),
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
        self.request_json(collection_method(route), &route.path(name), Some(request))
    }

    fn collection_status(&self, name: &str) -> CliResult<CollectionStatusResponse> {
        let route = CollectionApiEndpoint::CollectionStatus;
        self.request_json::<CollectionStatusResponse, ()>(
            collection_method(route),
            &route.path(name),
            None,
        )
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
        self.request_json::<CollectionWatcherResponse, ()>(
            collection_method(route),
            &route.path(name),
            None,
        )
    }

    fn update_collection_watcher(
        &self,
        name: &str,
        request: &CollectionWatcherUpdateRequest,
    ) -> CliResult<CollectionWatcherResponse> {
        let route = CollectionApiEndpoint::UpdateCollectionWatcher;
        self.request_json(collection_method(route), &route.path(name), Some(request))
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

    fn index_gc(&self, request: &IndexGcRequest) -> CliResult<IndexGcResponse> {
        self.request_json(Method::POST, "/api/index/gc", Some(request))
    }

    fn get_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse> {
        self.request_json::<TaskSummaryResponse, ()>(
            Method::GET,
            &format!("/api/tasks/{task_id}"),
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
        let mut request = self.client.get(&url);
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

    fn cancel_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse> {
        self.request_json::<TaskSummaryResponse, ()>(
            Method::POST,
            &format!("/api/tasks/{task_id}/cancel"),
            None,
        )
    }

    fn resume_task(&self, task_id: &str) -> CliResult<TaskSummaryResponse> {
        self.request_json::<TaskSummaryResponse, ()>(
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
            .apply_timeout(self.client.post(&url), RequestTimeoutPolicy::Finite)?
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
            &format!("/api/evidence/{evidence_id}"),
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

pub fn bind_to_base_url(bind: &str) -> String {
    if bind.starts_with("http://") || bind.starts_with("https://") {
        bind.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", bind.trim_end_matches('/'))
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
            || path == "/api/ingest"
            || path.starts_with("/api/ingest?")
            || path.starts_with("/api/ingest/")
            || path == "/api/reindex"
            || path == "/api/index/gc"
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
    if status == StatusCode::NOT_FOUND {
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
    (!message.is_empty()).then(|| message.to_string())
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
    let mut redacted = if let Ok(mut value) = serde_json::from_str::<Value>(body) {
        redact_json(&mut value);
        serde_json::to_string(&value).unwrap_or_else(|_| redact_json_like_text(body))
    } else {
        redact_json_like_text(body)
    };

    if truncated {
        redacted.push_str(HTTP_ERROR_TRUNCATION_MARKER);
    }
    redacted
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if is_secret_key(key) {
                    *child = Value::String("<redacted>".into());
                } else {
                    redact_json(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json(item);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
}

fn redact_json_like_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while let Some(relative_quote) = input[index..].find('"') {
        let quote = index + relative_quote;
        output.push_str(&input[index..quote]);

        let Some((key, key_end)) = parse_json_string(input, quote) else {
            output.push_str(&input[quote..]);
            return output;
        };
        output.push_str(&input[quote..key_end]);

        let Some(after_colon) = colon_after_key(input, key_end) else {
            index = key_end;
            continue;
        };

        if !is_secret_key(&key) {
            index = key_end;
            continue;
        }

        output.push_str(&input[key_end..after_colon]);
        let value_start = skip_ascii_whitespace(input, after_colon);
        output.push_str(&input[after_colon..value_start]);

        if input[value_start..].starts_with('"') {
            output.push_str("\"<redacted>\"");
            if let Some((_, value_end)) = parse_json_string(input, value_start) {
                index = value_end;
                continue;
            }
            return output;
        }

        output.push_str("\"<redacted>\"");
        index = next_json_value_boundary(input, value_start);
    }

    output.push_str(&input[index..]);
    output
}

fn parse_json_string(input: &str, quote: usize) -> Option<(String, usize)> {
    if !input[quote..].starts_with('"') {
        return None;
    }

    let mut escaped = false;
    let mut value = String::new();
    for (offset, character) in input[quote + 1..].char_indices() {
        let index = quote + 1 + offset;
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some((value, index + 1));
        } else {
            value.push(character);
        }
    }
    None
}

fn colon_after_key(input: &str, key_end: usize) -> Option<usize> {
    let colon = skip_ascii_whitespace(input, key_end);
    if input[colon..].starts_with(':') {
        Some(colon + 1)
    } else {
        None
    }
}

fn skip_ascii_whitespace(input: &str, mut index: usize) -> usize {
    while let Some(character) = input[index..].chars().next() {
        if !character.is_ascii_whitespace() {
            break;
        }
        index += character.len_utf8();
    }
    index
}

fn next_json_value_boundary(input: &str, start: usize) -> usize {
    input[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            matches!(character, ',' | '}' | ']' | '\n' | '\r').then_some(start + offset)
        })
        .unwrap_or(input.len())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn bind_to_base_url_adds_http_scheme() {
        assert_eq!(bind_to_base_url("127.0.0.1:7700"), "http://127.0.0.1:7700");
        assert_eq!(
            bind_to_base_url("http://127.0.0.1:7700/"),
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
        let server = TestServer::respond_once(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"src-1\"}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.add_source("/tmp/doc.pdf").unwrap();

        assert_eq!(response.id, "src-1");
        let request = server.request();
        assert!(request.starts_with("POST /api/sources HTTP/1.1"));
        assert!(request.contains("\"path\":\"/tmp/doc.pdf\""));
    }

    #[test]
    fn http_collection_routes_are_plumbed_from_shared_inventory() {
        let collection = concat!(
            "{\"collection\":{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"},",
            "\"roots\":[],\"members\":[]}"
        );
        let collection_list = "[{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"}]";
        let sync = concat!(
            "{\"report\":{\"member_count\":1,\"added\":1,\"removed\":0,\"unchanged\":0,",
            "\"scanned_roots\":1,\"max_depth\":32,\"skipped\":[]}}"
        );
        let status = concat!(
            "{\"status\":{\"collection\":{\"name\":\"articles\",\"created_at\":\"1\",",
            "\"updated_at\":\"2\"},\"root_count\":1,\"member_count\":1}}"
        );
        let watcher_status = concat!(
            "{\"collection_name\":\"articles\",\"watch_enabled\":true,",
            "\"auto_index_enabled\":false,\"active\":true,\"ignored_by_config\":false,",
            "\"watched_root_count\":1,\"pending_event_count\":0}"
        );
        let watcher = format!(
            "{{\"collection\":{{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"}},\
             \"watcher\":{watcher_status}}}"
        );
        let watchers = format!("{{\"watchers\":[{watcher_status}]}}");
        let server = TestServer::respond_many(vec![
            json_response("201 Created", collection),
            json_response("200 OK", collection),
            json_response("200 OK", collection_list),
            json_response("200 OK", collection),
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
            json_response("200 OK", sync),
            json_response("200 OK", status),
            json_response("200 OK", watchers.as_str()),
            json_response("200 OK", watcher.as_str()),
            json_response("200 OK", watcher.as_str()),
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        assert_eq!(
            client
                .create_collection(&CreateCollectionRequest {
                    name: "articles".into(),
                    ignore_patterns: vec!["drafts/".into()],
                })
                .unwrap()
                .collection
                .name,
            "articles"
        );
        assert_eq!(
            client
                .add_collection_root(
                    "articles",
                    &AddCollectionRootRequest {
                        path: "/tmp/articles".into(),
                    },
                )
                .unwrap()
                .collection
                .name,
            "articles"
        );
        assert_eq!(client.list_collections().unwrap()[0].name, "articles");
        assert_eq!(
            client.get_collection("articles").unwrap().collection.name,
            "articles"
        );
        client.delete_collection("articles").unwrap();
        assert_eq!(
            client
                .sync_collection(
                    "articles",
                    &CollectionSyncRequest {
                        paths: Vec::new(),
                        max_depth: Some(7),
                    },
                )
                .unwrap()
                .report
                .member_count,
            1
        );
        assert_eq!(
            client
                .collection_status("articles")
                .unwrap()
                .status
                .member_count,
            1
        );
        assert_eq!(
            client.list_collection_watcher_statuses().unwrap().watchers[0].collection_name,
            "articles"
        );
        assert!(
            client
                .collection_watcher_status("articles")
                .unwrap()
                .watcher
                .active
        );
        assert!(
            client
                .update_collection_watcher(
                    "articles",
                    &CollectionWatcherUpdateRequest {
                        enabled: true,
                        auto_index_enabled: Some(false),
                    },
                )
                .unwrap()
                .watcher
                .watch_enabled
        );

        let requests = server.requests();
        assert_collection_request(&requests[0], CollectionApiEndpoint::CreateCollection, None);
        assert!(requests[0].contains("\"ignore_patterns\":[\"drafts/\"]"));
        assert_collection_request(
            &requests[1],
            CollectionApiEndpoint::AddCollectionRoot,
            Some("articles"),
        );
        assert!(requests[1].contains("\"path\":\"/tmp/articles\""));
        assert_collection_request(&requests[2], CollectionApiEndpoint::ListCollections, None);
        assert_collection_request(
            &requests[3],
            CollectionApiEndpoint::GetCollection,
            Some("articles"),
        );
        assert_collection_request(
            &requests[4],
            CollectionApiEndpoint::DeleteCollection,
            Some("articles"),
        );
        assert_collection_request(
            &requests[5],
            CollectionApiEndpoint::SyncCollection,
            Some("articles"),
        );
        assert!(requests[5].contains("\"max_depth\":7"));
        assert_collection_request(
            &requests[6],
            CollectionApiEndpoint::CollectionStatus,
            Some("articles"),
        );
        assert_collection_request(
            &requests[7],
            CollectionApiEndpoint::ListCollectionWatcherStatuses,
            None,
        );
        assert_collection_request(
            &requests[8],
            CollectionApiEndpoint::CollectionWatcherStatus,
            Some("articles"),
        );
        assert_collection_request(
            &requests[9],
            CollectionApiEndpoint::UpdateCollectionWatcher,
            Some("articles"),
        );
        assert!(requests[9].contains("\"enabled\":true"));
    }

    #[test]
    fn http_ingest_force_uses_query_parameter() {
        let server = TestServer::respond_once(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"ingested\":2}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.ingest(None, true, None, false).unwrap();

        assert_eq!(response.ingested, 2);
        assert!(server
            .request()
            .starts_with("POST /api/ingest?force=true HTTP/1.1"));
    }

    #[test]
    fn http_ingest_profile_vectors_only_uses_query_parameters() {
        let server = TestServer::respond_once(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"ingested\":1}",
        );
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client
            .ingest(Some("src-1"), false, Some("alt.profile"), true)
            .unwrap();

        assert_eq!(response.ingested, 1);
        assert!(server.request().starts_with(
            "POST /api/ingest/src-1?embedding_profile_id=alt.profile&vectors_only=true HTTP/1.1"
        ));
    }

    #[test]
    fn http_reindex_posts_json_to_daemon() {
        let server = TestServer::respond_many(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"reindexed\":1}".to_string(),
            "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"task_id\":\"task-1\"}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"reindexed\":2}".to_string(),
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

    #[test]
    fn http_index_gc_posts_dry_run_request() {
        let body = concat!(
            "{\"dry_run\":true,",
            "\"policy\":{\"retain_previous_generations\":2,\"stale_staging_seconds\":86400},",
            "\"plan\":{\"entries\":[{\"path\":\"/tmp/verbatim/indexes/profiles/default/gen-1\",",
            "\"kind\":\"generation\",\"profile_id\":\"default\",\"generation\":1,",
            "\"approximate_bytes\":10,\"reason\":\"old\"}],\"skipped\":[],",
            "\"approximate_reclaim_bytes\":10},",
            "\"apply\":{\"removed\":[],\"reclaimed_bytes\":0}}"
        );
        let server = TestServer::respond_many(vec![json_response("200 OK", body)]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        let response = client.index_gc(&IndexGcRequest { dry_run: true }).unwrap();

        assert!(response.dry_run);
        assert_eq!(response.plan.entries.len(), 1);
        let request = server.request();
        assert!(request.starts_with("POST /api/index/gc HTTP/1.1"));
        assert!(request.contains("\"dry_run\":true"));
    }

    #[test]
    fn http_task_routes_are_plumbed() {
        let task_summary = concat!(
            "{\"task\":{\"id\":\"task-1\",\"kind\":\"ask\",\"status\":\"succeeded\",",
            "\"created_at\":\"1\",\"updated_at\":\"2\",\"started_at\":\"1\",\"finished_at\":\"2\",",
            "\"request\":{\"question_chars\":4},\"result\":{\"citation_count\":1},\"error\":null},",
            "\"spans\":[{\"sequence\":1,\"task_id\":\"task-1\",\"phase\":\"chat\",",
            "\"started_at\":\"1\",\"duration_ms\":5,\"metadata\":{\"citation_count\":1}}]}"
        );
        let server = TestServer::respond_many(vec![
            "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"task_id\":\"task-1\"}".to_string(),
            "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"task_id\":\"task-2\"}".to_string(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_summary}"
            ),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"events\":[{\"sequence\":2,\"task_id\":\"task-1\",\"event_type\":\"phase\",\"message\":\"done\",\"payload\":{},\"created_at\":\"2\"}]}".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: task\ndata: {\"task\":{\"id\":\"task-1\",\"kind\":\"ask\",\"status\":\"succeeded\",\"created_at\":\"1\",\"updated_at\":\"2\",\"started_at\":\"1\",\"finished_at\":\"2\",\"request\":{},\"result\":{},\"error\":null},\"events\":[],\"spans\":[],\"terminal\":true}\n\n".to_string(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_summary}"
            ),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{task_summary}"
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
        };

        assert_eq!(client.submit_ask_task(&ask).unwrap().task_id, "task-1");
        assert_eq!(
            client
                .submit_ingest_task(Some("src-1"), false, None, false)
                .unwrap()
                .task_id,
            "task-2"
        );
        assert_eq!(client.get_task("task-1").unwrap().task.id.0, "task-1");
        assert_eq!(
            client.get_task_events("task-1", Some(1)).unwrap().events[0].sequence,
            2
        );
        let mut stdout = Vec::new();
        client
            .wait_task("task-1", Some(2), TaskWaitTimeout::Unbounded, &mut stdout)
            .unwrap();
        assert!(String::from_utf8(stdout).unwrap().contains("Task: task-1"));
        assert_eq!(
            client.cancel_task("task-1").unwrap().task.status.as_str(),
            "succeeded"
        );
        assert_eq!(
            client.resume_task("task-1").unwrap().task.status.as_str(),
            "succeeded"
        );

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
        let server = TestServer::respond_slow_stream(
            concat!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                "event: task\n",
                "data: {\"task\":{\"id\":\"task-1\",\"kind\":\"ask\",\"status\":\"running\",",
                "\"created_at\":\"1\",\"updated_at\":\"2\",\"started_at\":\"1\",\"finished_at\":null,",
                "\"request\":{},\"result\":null,\"error\":null,",
                "\"progress\":{\"phase\":{\"name\":\"chat\",\"started_at\":\"1\",\"elapsed_ms\":100},",
                "\"recent_status\":\"streaming\"}},\"events\":[],\"spans\":[],\"terminal\":false}\n\n",
            ),
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
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"ev-1\",\"source_id\":\"src-1\",\"kind\":\"text\",\"derived_from\":null,\"locator\":\"PDF p.1 para.1\",\"structured_locator\":{\"type\":\"Pdf\",\"page\":1,\"paragraph\":1,\"bbox\":null},\"text\":\"quoted\",\"heading_path\":[],\"position\":0,\"image_artifact\":null}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"config\":{\"daemon\":{\"bind\":\"x\"}},\"reload\":{\"active_config_path\":\"/tmp/config.toml\",\"loaded_at\":\"1\",\"last_applied_reload_safe_keys\":[],\"last_restart_required_keys\":[]}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"status\":\"ok\"}",
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        assert_eq!(client.get_evidence("ev-1").unwrap().id, "ev-1");
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
