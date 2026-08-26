//! Contract tests for stable SDK client trait contracts (SDK-001 / #355).

use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::index_publication::PointerEpoch;
use crate::pagination::{
    PaginationMode, SnapshotPageRequest, SnapshotPageRequestFields, SnapshotPageResponse,
};
use crate::storage_ports::{PageCursor, StorageGeneration};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, ContextPackEnvelope, ContextPackFields,
    DerivedArtifactEnvelope, DerivedArtifactFields, DerivedArtifactKind, EvidencePackEnvelope,
    EvidencePackFields, QueryPlanEnvelope, QueryPlanFields, WireArtifactKind, WorkflowEnvelope,
    WorkflowEnvelopeFields, WorkflowPhase, WIRE_SCHEMA_VERSION,
};

#[path = "sdk_tests/workflow_wire.rs"]
mod workflow_wire;

fn sample_query_plan() -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp-sdk-1".into(),
        query_text: "what is the retention policy?".into(),
        steps: vec!["lexical".into(), "vector".into()],
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_evidence_pack(plan_hash: &str) -> EvidencePackEnvelope {
    EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: "ep-sdk-1".into(),
        evidence_unit_ids: vec!["eu-a".into(), "eu-b".into()],
        query_plan_hash: plan_hash.into(),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_context_pack(evidence_hash: &str) -> ContextPackEnvelope {
    ContextPackEnvelope::new(ContextPackFields {
        artifact_id: "cp-sdk-1".into(),
        evidence_pack_hash: evidence_hash.into(),
        selected_unit_ids: vec!["eu-a".into()],
        model_fingerprint: Some("model-fp-1".into()),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_derived(source_hash: &str) -> DerivedArtifactEnvelope {
    DerivedArtifactEnvelope::new(DerivedArtifactFields {
        artifact_id: "da-sdk-1".into(),
        kind: DerivedArtifactKind::DraftAnswer,
        source_pack_hash: source_hash.into(),
        model_fingerprint: "model-fp-1".into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap()
}

fn sample_workflow(plan_hash: &str) -> WorkflowEnvelope {
    WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: "wf-sdk-1".into(),
        phase: WorkflowPhase::Retrieving,
        query_plan_hash: plan_hash.into(),
        evidence_pack_hash: None,
        context_pack_hash: None,
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

#[derive(serde::Serialize)]
struct ContextPackIdentityBody {
    evidence_pack_hash: String,
    selected_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_fingerprint: Option<String>,
}

fn context_pack_with_selected_ids(
    evidence_pack_hash: &str,
    selected_unit_ids: Vec<String>,
) -> ContextPackEnvelope {
    let mut context_pack = sample_context_pack(evidence_pack_hash);
    context_pack.selected_unit_ids = selected_unit_ids;
    let body = ContextPackIdentityBody {
        evidence_pack_hash: context_pack.evidence_pack_hash.clone(),
        selected_unit_ids: context_pack.selected_unit_ids.clone(),
        model_fingerprint: context_pack.model_fingerprint.clone(),
    };
    context_pack.header.identity = CanonicalIdentity::from_body(
        WireArtifactKind::ContextPack,
        WIRE_SCHEMA_VERSION,
        context_pack.header.identity.artifact_id.clone(),
        &encode_wire_document(&body).unwrap(),
    )
    .unwrap();
    context_pack
}

fn workflow_for_context(
    query_plan: &QueryPlanEnvelope,
    context_pack: &ContextPackEnvelope,
) -> WorkflowEnvelope {
    WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: "wf-sdk-live".into(),
        phase: WorkflowPhase::Assembling,
        query_plan_hash: query_plan.header.identity.content_hash.as_str().into(),
        evidence_pack_hash: Some(context_pack.evidence_pack_hash.clone()),
        context_pack_hash: Some(context_pack.header.identity.content_hash.as_str().into()),
        generation: None,
        profile_ref: None,
    })
    .unwrap()
}

fn live_workflow_request(
    query_plan: QueryPlanEnvelope,
    evidence_pack: Option<EvidencePackEnvelope>,
    context_pack: Option<ContextPackEnvelope>,
) -> WorkflowRunRequest {
    WorkflowRunRequest::new(
        "wf-sdk-live",
        WorkflowPhase::Assembling,
        query_plan,
        evidence_pack,
        context_pack,
    )
    .unwrap()
}

fn workflow_request_with_context_ids(selected_unit_ids: Vec<String>) -> WorkflowRunRequest {
    let query_plan = sample_query_plan();
    let evidence_pack = sample_evidence_pack(query_plan.header.identity.content_hash.as_str());
    let context_pack = context_pack_with_selected_ids(
        evidence_pack.header.identity.content_hash.as_str(),
        selected_unit_ids,
    );
    WorkflowRunRequest {
        workflow: workflow_for_context(&query_plan, &context_pack),
        query_plan,
        evidence_pack: None,
        context_pack: Some(context_pack),
        idempotency_key: None,
    }
}

fn sample_page_request(query_plan_hash: &str) -> SnapshotPageRequest {
    SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::RankedSearch,
        limit: 10,
        query_plan_hash: query_plan_hash.into(),
        principal: "user:alice".into(),
        publication_generation: StorageGeneration::new(7),
        profile_ref: "profile:default".into(),
        policy_version: "policy-v3".into(),
        cursor: None,
        pointer_epoch: None,
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[test]
fn sdk_config_construction_and_validation() {
    let cfg = SdkConfig::with_endpoint("https://verbatim.example/api").unwrap();
    assert_eq!(cfg.timeout, Duration::from_secs(DEFAULT_SDK_TIMEOUT_SECS));
    assert_eq!(cfg.user_agent, DEFAULT_SDK_USER_AGENT);
    assert!(!cfg.has_auth_token());
    assert!(cfg.capability_cache.is_empty());

    let cfg = cfg.with_auth_token("tok-abc").unwrap();
    assert!(cfg.has_auth_token());
    let debug = format!("{cfg:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("tok-abc"));

    assert!(SdkConfig::with_endpoint("ftp://x").is_err());
    assert!(SdkConfig::with_endpoint("https://").is_err());
    assert!(SdkConfig::with_endpoint("https://host with space").is_err());
    assert!(SdkConfig::with_endpoint("").is_err());
    assert!(cfg.clone().with_timeout(Duration::from_secs(0)).is_err());
    assert!(cfg.clone().with_auth_token("  ").is_err());
    assert!(cfg.clone().with_user_agent("").is_err());
}

#[test]
fn sdk_config_json_round_trip() {
    let cfg = SdkConfig::with_endpoint("http://127.0.0.1:8080")
        .unwrap()
        .with_auth_token("secret")
        .unwrap()
        .with_timeout(Duration::from_secs(12))
        .unwrap()
        .with_user_agent("verbatim-sdk-test/1")
        .unwrap();
    let bytes = serde_json::to_vec(&cfg).unwrap();
    let back: SdkConfig = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back.endpoint, cfg.endpoint);
    assert_eq!(back.auth_token, cfg.auth_token);
    assert_eq!(back.timeout, cfg.timeout);
    assert_eq!(back.user_agent, cfg.user_agent);
    back.validate().unwrap();
}

#[test]
fn sdk_config_fields_debug_redacts_auth_token() {
    let fields = SdkConfigFields {
        endpoint: "https://verbatim.example/api".into(),
        auth_token: Some("super-secret-bearer-token".into()),
        timeout: Duration::from_secs(DEFAULT_SDK_TIMEOUT_SECS),
        user_agent: DEFAULT_SDK_USER_AGENT.to_string(),
        capability_cache: CapabilityCache::empty(),
    };
    let debug = format!("{fields:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("super-secret-bearer-token"));
    assert!(!debug.contains("bearer"));
}

// ---------------------------------------------------------------------------
// Capability negotiation
// ---------------------------------------------------------------------------

#[test]
fn capability_negotiation_intersects_and_fails_closed() {
    let advertised = SdkCapabilityDescriptor::new([
        SdkCapabilityKind::Search,
        SdkCapabilityKind::Retrieve,
        SdkCapabilityKind::Evidence,
    ]);
    let ok = CapabilityNegotiation::new(
        [SdkCapabilityKind::Search, SdkCapabilityKind::Retrieve],
        advertised.clone(),
    )
    .unwrap()
    .negotiate()
    .unwrap();
    assert!(ok.supports(SdkCapabilityKind::Search));
    assert!(ok.supports(SdkCapabilityKind::Retrieve));
    assert!(!ok.supports(SdkCapabilityKind::Generate));

    let err = CapabilityNegotiation::new([SdkCapabilityKind::Generate], advertised)
        .unwrap()
        .negotiate()
        .unwrap_err();
    assert_eq!(err.class_name(), "unsupported");
    assert!(!err.is_retryable());
}

#[test]
fn capability_require_and_cache() {
    let advertised = SdkCapabilityDescriptor::all_supported()
        .with_server_label("daemon-a")
        .unwrap();
    let negotiation = CapabilityNegotiation::new([], advertised.clone()).unwrap();
    negotiation
        .require(SdkCapabilityKind::Workflow, "run_workflow")
        .unwrap();

    let limited = SdkCapabilityDescriptor::new([SdkCapabilityKind::Search]);
    let limited_neg = CapabilityNegotiation::new([], limited).unwrap();
    let err = limited_neg
        .require(SdkCapabilityKind::Generate, "generate")
        .unwrap_err();
    assert_eq!(err.class_name(), "unsupported");

    let cache = CapabilityCache::from_descriptor(advertised)
        .unwrap()
        .with_refreshed_at(1_700_000_000);
    assert_eq!(cache.supports(SdkCapabilityKind::Task), Some(true));
    cache.validate().unwrap();
}

#[test]
fn capability_descriptor_json_round_trip_and_schema_fail_closed() {
    let desc =
        SdkCapabilityDescriptor::new([SdkCapabilityKind::Capabilities, SdkCapabilityKind::Search]);
    let bytes = serde_json::to_vec(&desc).unwrap();
    let back = decode_sdk_capability_descriptor_json(&bytes).unwrap();
    assert_eq!(back, desc);

    let mut bad = desc.clone();
    bad.schema_version = 99;
    let bad_bytes = serde_json::to_vec(&bad).unwrap();
    let err = decode_sdk_capability_descriptor_json(&bad_bytes).unwrap_err();
    assert_eq!(err.class_name(), "compatibility");
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn client_error_classes_and_retryability() {
    assert!(ClientError::transport("search", "reset").is_retryable());
    assert!(ClientError::timeout("retrieve", "deadline").is_retryable());
    assert!(!ClientError::auth("missing token").is_retryable());
    assert!(!ClientError::validation("empty id").is_retryable());
    assert!(!ClientError::compatibility("schema").is_retryable());
    assert!(
        !ClientError::unsupported(SdkCapabilityKind::Artifact, "get_artifact", "missing")
            .is_retryable()
    );
    assert_eq!(
        ClientError::not_found("source", "src-1").class_name(),
        "not_found"
    );
}

// ---------------------------------------------------------------------------
// Operation envelopes
// ---------------------------------------------------------------------------

#[test]
fn source_upload_and_task_envelopes_validate() {
    let up = SourceUploadRequest::new("file:///docs/a.md").unwrap();
    up.validate().unwrap();
    assert!(SourceUploadRequest::new("").is_err());

    let resp = SourceUploadResponse {
        source_id: "src-1".into(),
        content_hash: Some("abc123".into()),
        accepted: true,
    };
    resp.validate().unwrap();

    let submit = TaskSubmitRequest::new("ingest").unwrap();
    submit.validate().unwrap();
    let get = TaskGetRequest::new("task-1").unwrap();
    get.validate().unwrap();
    let get_resp = TaskGetResponse {
        task_id: "task-1".into(),
        status: "completed".into(),
        result_hash: Some("deadbeef".into()),
    };
    get_resp.validate().unwrap();
}

#[test]
fn search_request_binds_query_plan_hash_to_page() {
    let page = sample_page_request("qp-hash-1");
    let req = SearchRequest::new("qp-hash-1", page).unwrap();
    req.validate().unwrap();

    let mismatched = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::RankedSearch,
        limit: 5,
        query_plan_hash: "other".into(),
        principal: "user:alice".into(),
        publication_generation: StorageGeneration::new(1),
        profile_ref: "profile:default".into(),
        policy_version: "policy-v1".into(),
        cursor: None,
        pointer_epoch: None,
    })
    .unwrap();
    let err = SearchRequest::new("qp-hash-1", mismatched).unwrap_err();
    assert_eq!(err.class_name(), "validation");
}

#[test]
fn r_a_g_operation_envelopes_construct() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let ep = sample_evidence_pack(&plan_hash);
    let ep_hash = ep.header.identity.content_hash.as_str().to_string();
    let cp = sample_context_pack(&ep_hash);
    let cp_hash = cp.header.identity.content_hash.as_str().to_string();
    let da = sample_derived(&cp_hash);

    RetrieveRequest::new(plan).unwrap().validate().unwrap();
    EvidenceGetRequest::new(&ep_hash)
        .unwrap()
        .validate()
        .unwrap();
    ContextBuildRequest::new(ep).unwrap().validate().unwrap();
    GenerateRequest::new(cp).unwrap().validate().unwrap();
    VerifyRequest::new(da, &ep_hash)
        .unwrap()
        .validate()
        .unwrap();
    WorkflowRunRequest::new(
        "wf-sdk-1",
        WorkflowPhase::Retrieving,
        sample_query_plan(),
        None,
        None,
    )
    .unwrap()
    .validate()
    .unwrap();

    let art = ArtifactRef::new("derived_artifact", "da-sdk-1")
        .unwrap()
        .with_content_hash("abc")
        .unwrap();
    ArtifactGetRequest::new(art).unwrap().validate().unwrap();
}

// ---------------------------------------------------------------------------
// CursorIterator
// ---------------------------------------------------------------------------

struct ScriptedFetcher {
    pages: Mutex<Vec<SnapshotPageResponse<String>>>,
}

impl CursorPageFetcher<String> for ScriptedFetcher {
    fn fetch_page(
        &self,
        _request: SnapshotPageRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = ClientResult<SnapshotPageResponse<String>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let mut pages = self.pages.lock().unwrap();
            if pages.is_empty() {
                return Err(ClientError::validation("no scripted pages left"));
            }
            Ok(pages.remove(0))
        })
    }
}

#[tokio::test]
async fn cursor_iterator_walks_pages_and_exhausts() {
    let gen = StorageGeneration::new(7);
    let first: SnapshotPageResponse<String> = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        gen,
        vec!["a".to_string(), "b".to_string()],
        Some(PageCursor::new("cursor-page-2").unwrap()),
        false,
        None,
    );
    let second: SnapshotPageResponse<String> = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        gen,
        vec!["c".to_string()],
        None,
        true,
        Some(3),
    );
    let fetcher = ScriptedFetcher {
        pages: Mutex::new(vec![first, second]),
    };

    let request = sample_page_request("qp-hash-1");
    let mut iter = CursorIterator::from_request(&request).unwrap();
    assert!(iter.has_next());

    let page1 = iter.next_page(&fetcher).await.unwrap().unwrap();
    assert_eq!(page1.items, vec!["a".to_string(), "b".to_string()]);
    assert!(iter.has_next());

    let page2 = iter.next_page(&fetcher).await.unwrap().unwrap();
    assert_eq!(page2.items, vec!["c".to_string()]);
    assert!(iter.is_exhausted());
    assert!(iter.next_page(&fetcher).await.unwrap().is_none());
}

#[test]
fn cursor_iterator_rejects_mode_and_generation_mismatch() {
    let request = sample_page_request("qp-hash-1");
    let mut iter = CursorIterator::from_request(&request).unwrap();

    let mode_bad: SnapshotPageResponse<String> = SnapshotPageResponse::page(
        PaginationMode::ExhaustiveEnumeration,
        StorageGeneration::new(7),
        vec!["x".to_string()],
        None,
        true,
        Some(1),
    );
    let err = iter.advance_with(&mode_bad).unwrap_err();
    assert_eq!(err.class_name(), "pagination");
    assert!(err.to_string().contains("mode"));

    let mut iter = CursorIterator::from_request(&request).unwrap();
    let gen_bad: SnapshotPageResponse<String> = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        StorageGeneration::new(99),
        vec!["x".to_string()],
        None,
        true,
        Some(1),
    );
    let err = iter.advance_with(&gen_bad).unwrap_err();
    assert_eq!(err.class_name(), "pagination");
    assert!(err.to_string().contains("generation"));
}

#[test]
fn cursor_iterator_preserves_pointer_epoch_across_pages() {
    let epoch = PointerEpoch::new(4);
    let request = SnapshotPageRequest::new(SnapshotPageRequestFields {
        mode: PaginationMode::RankedSearch,
        limit: 10,
        query_plan_hash: "qp-hash-epoch".into(),
        principal: "user:alice".into(),
        publication_generation: StorageGeneration::new(7),
        profile_ref: "profile:default".into(),
        policy_version: "policy-v3".into(),
        cursor: None,
        pointer_epoch: Some(epoch),
    })
    .unwrap();

    let mut iter = CursorIterator::from_request(&request).unwrap();
    assert_eq!(iter.pointer_epoch, Some(epoch));

    let first_req = iter.next_request().unwrap().unwrap();
    assert_eq!(first_req.pointer_epoch, Some(epoch));

    let page: SnapshotPageResponse<String> = SnapshotPageResponse::page(
        PaginationMode::RankedSearch,
        StorageGeneration::new(7),
        vec!["a".to_string()],
        Some(PageCursor::new("cursor-page-2").unwrap()),
        false,
        None,
    );
    iter.advance_with(&page).unwrap();
    assert_eq!(iter.pointer_epoch, Some(epoch));
    assert!(!iter.is_exhausted());

    let cont = iter.next_request().unwrap().unwrap();
    assert_eq!(cont.pointer_epoch, Some(epoch));
    assert_eq!(
        cont.cursor.as_ref().map(|c| c.0.as_str()),
        Some("cursor-page-2")
    );
}

// ---------------------------------------------------------------------------
// Trait surface smoke (in-memory stub)
// ---------------------------------------------------------------------------

struct StubClient {
    caps: SdkCapabilityDescriptor,
}

#[async_trait::async_trait]
impl VerbatimClient for StubClient {
    async fn discover_capabilities(&self) -> ClientResult<SdkCapabilityDescriptor> {
        Ok(self.caps.clone())
    }

    async fn upload_source(
        &self,
        request: SourceUploadRequest,
    ) -> ClientResult<SourceUploadResponse> {
        request.validate()?;
        Ok(SourceUploadResponse {
            source_id: "src-stub".into(),
            content_hash: request.content_hash,
            accepted: true,
        })
    }

    async fn search(&self, request: SearchRequest) -> ClientResult<SearchResponse> {
        request.validate()?;
        Ok(SearchResponse {
            page: SnapshotPageResponse::empty(
                request.page.mode,
                request.page.publication_generation,
            ),
        })
    }

    async fn retrieve(&self, request: RetrieveRequest) -> ClientResult<RetrieveResponse> {
        request.validate()?;
        let hash = request
            .query_plan
            .header
            .identity
            .content_hash
            .as_str()
            .to_string();
        Ok(RetrieveResponse {
            evidence_pack: sample_evidence_pack(&hash),
        })
    }

    async fn resolve(&self, request: ResolveRequest) -> ClientResult<ResolveResponse> {
        request.validate()?;
        Ok(ResolveResponse {
            artifact_ref: request.artifact_ref.clone(),
            resolved_locator: format!("verbatim://{}", request.artifact_ref.id),
        })
    }

    async fn get_evidence(&self, request: EvidenceGetRequest) -> ClientResult<EvidenceGetResponse> {
        request.validate()?;
        let plan = sample_query_plan();
        let plan_hash = plan.header.identity.content_hash.as_str().to_string();
        Ok(EvidenceGetResponse {
            evidence_pack: sample_evidence_pack(&plan_hash),
        })
    }

    async fn build_context(
        &self,
        request: ContextBuildRequest,
    ) -> ClientResult<ContextBuildResponse> {
        request.validate()?;
        let hash = request
            .evidence_pack
            .header
            .identity
            .content_hash
            .as_str()
            .to_string();
        Ok(ContextBuildResponse {
            context_pack: sample_context_pack(&hash),
        })
    }

    async fn generate(&self, request: GenerateRequest) -> ClientResult<GenerateResponse> {
        request.validate()?;
        let hash = request
            .context_pack
            .header
            .identity
            .content_hash
            .as_str()
            .to_string();
        Ok(GenerateResponse {
            artifact: sample_derived(&hash),
        })
    }

    async fn verify(&self, request: VerifyRequest) -> ClientResult<VerifyResponse> {
        request.validate()?;
        Ok(VerifyResponse {
            ok: true,
            detail: None,
        })
    }

    async fn run_workflow(&self, request: WorkflowRunRequest) -> ClientResult<WorkflowRunResponse> {
        request.validate()?;
        WorkflowRunResponse::new("run-1", request)
    }

    async fn submit_task(&self, request: TaskSubmitRequest) -> ClientResult<TaskSubmitResponse> {
        request.validate()?;
        Ok(TaskSubmitResponse {
            task_id: "task-stub".into(),
        })
    }

    async fn get_task(&self, request: TaskGetRequest) -> ClientResult<TaskGetResponse> {
        request.validate()?;
        Ok(TaskGetResponse {
            task_id: request.task_id,
            status: "queued".into(),
            result_hash: None,
        })
    }

    async fn get_artifact(&self, request: ArtifactGetRequest) -> ClientResult<ArtifactGetResponse> {
        request.validate()?;
        Ok(ArtifactGetResponse {
            artifact_ref: request.artifact_ref,
            body_hash: Some("bodyhash".into()),
        })
    }
}

#[tokio::test]
async fn verbatim_client_stub_round_trip() {
    let client = StubClient {
        caps: SdkCapabilityDescriptor::all_supported(),
    };
    let caps = client.discover_capabilities().await.unwrap();
    assert!(caps.supports(SdkCapabilityKind::Search));

    let negotiated = client
        .negotiate_capabilities(
            &[SdkCapabilityKind::Search, SdkCapabilityKind::Retrieve],
            &caps,
        )
        .await
        .unwrap();
    assert!(negotiated.supports(SdkCapabilityKind::Search));

    client
        .require_capability(&caps, SdkCapabilityKind::Generate, "generate")
        .unwrap();

    let up = client
        .upload_source(SourceUploadRequest::new("file:///x.md").unwrap())
        .await
        .unwrap();
    assert_eq!(up.source_id, "src-stub");

    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let page = sample_page_request(&plan_hash);
    let search = client
        .search(SearchRequest::new(plan_hash.clone(), page).unwrap())
        .await
        .unwrap();
    assert!(search.page.exhausted);

    let retrieved = client
        .retrieve(RetrieveRequest::new(plan).unwrap())
        .await
        .unwrap();
    retrieved.validate().unwrap();

    let art = ArtifactRef::new("query_plan", "qp-sdk-1").unwrap();
    let resolved = client
        .resolve(ResolveRequest::new(art.clone()).unwrap())
        .await
        .unwrap();
    assert!(resolved.resolved_locator.contains("qp-sdk-1"));

    let ep_hash = retrieved
        .evidence_pack
        .header
        .identity
        .content_hash
        .as_str()
        .to_string();
    client
        .get_evidence(EvidenceGetRequest::new(&ep_hash).unwrap())
        .await
        .unwrap();
    let ctx = client
        .build_context(ContextBuildRequest::new(retrieved.evidence_pack).unwrap())
        .await
        .unwrap();
    let gen = client
        .generate(GenerateRequest::new(ctx.context_pack).unwrap())
        .await
        .unwrap();
    let verified = client
        .verify(VerifyRequest::new(gen.artifact, &ep_hash).unwrap())
        .await
        .unwrap();
    assert!(verified.ok);

    let run = client
        .run_workflow(
            WorkflowRunRequest::new(
                "wf-sdk-1",
                WorkflowPhase::Retrieving,
                sample_query_plan(),
                None,
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(run.run_id, "run-1");

    let task = client
        .submit_task(TaskSubmitRequest::new("ingest").unwrap())
        .await
        .unwrap();
    client
        .get_task(TaskGetRequest::new(task.task_id).unwrap())
        .await
        .unwrap();
    client
        .get_artifact(ArtifactGetRequest::new(art).unwrap())
        .await
        .unwrap();
}
