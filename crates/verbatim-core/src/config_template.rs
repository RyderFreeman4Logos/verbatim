//! Default configuration template text.
//!
//! The template is the canonical `verbatim init` default and doubles as the
//! parsing fixture for default values including the SQLite store durability
//! profile. Keeping it in a sibling module leaves `config.rs` focused on
//! parsing, validation, and reload semantics.

/// The TOML template written by `verbatim init` and used as the default-value
/// parsing fixture.
pub(super) const DEFAULT_CONFIG_TEMPLATE: &str = r#"# Verbatim configuration
# See: https://github.com/RyderFreeman4Logos/verbatim

[store]
# SQLite metadata/text, vectors, and BM25 live under this data directory.
path = "~/.local/share/verbatim"
# durable: RPO=0 for acknowledged SQLite commits; balanced: faster WAL writes
# with an unbounded power-failure RPO; ephemeral: scratch data only.
durability = "balanced"

[parser]
# pdf_oxide is the default for born-digital PDFs; scanned/image-only PDFs need OCR.
default = "pdf_oxide"   # pdf_oxide or pdfplumber

[parser.image_artifacts]
# Image artifact limits keep extracted images bounded.
max_images_per_source = 512
# Per-image byte cap for image artifacts.
max_bytes_per_image = 16777216
# Total image-artifact bytes stored per source.
max_total_bytes_per_source = 268435456
# Width, height, and pixel caps keep scanned/image-only inputs bounded.
max_image_width = 10000
max_image_height = 10000
max_image_pixels = 100000000

[embedding]
# Endpoint/model settings point at the embedding service used for ingest and retrieval.
profile_id = "default"
enabled = true
# Keep provider/base_url/model aligned with the embedding endpoint you run.
provider = "openai_compatible"
base_url = "http://127.0.0.1:8002/v1"
model = "Qwen/Qwen3-Embedding-8B"
dimension = 4096
normalize = true
query_instruction = "Given a user's question about a document, retrieve the exact passages that directly support a grounded answer with source-level citations."
document_instruction = ""
batch_size = 16
# Timeout for each embedding request.
timeout_seconds = 120
# Leave api_key empty unless the embedding service requires auth.
api_key = ""
capability_cache_ttl_seconds = 60
# concurrency and queue timeout bound endpoint backpressure.
max_concurrent_requests = 4
queue_timeout_seconds = 300

[embedding.retry]
# Retry/backoff applies to the embedding endpoint.
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[retrieval]
# dense_top_k and bm25_top_k bound the candidate pool before RRF fusion.
dense_top_k = 80
bm25_top_k = 50
rrf_k = 60
# default_limit is the default page size for answer/context output.
default_limit = 12
default_page_size = 1

[vector_index]
# low_memory scans SQLite-stored vectors at query time and avoids resident HNSW.
# resident_hnsw loads the published local HNSW index into daemon memory.
residency = "low_memory"

[graph]
# Enable graph expansion for multi-hop/link traversal; leave it off for lexical+dense-only flows.
enabled = true
max_hops = 1
max_expanded_chunks = 30
max_neighbors_per_seed = 6
edge_types = ["parent", "previous", "next", "section_contains", "page_contains_image", "image_near_text", "markdown_links_to"]

[graph.extraction]
# Enable only when you want graph extraction to build new graph data during ingest.
enabled = false
max_chunks = 8
max_chunk_chars = 3000
max_entities = 24
max_relationships = 32
max_claims = 32
max_retries = 1
max_output_tokens = 2048
max_response_chars = 32768
max_error_chars = 256

[graph.global_search]
# Enable for graph-wide summaries; drift controls follow-up subqueries.
enabled = false
max_communities = 128
max_report_claims = 12
max_report_chars = 4000
max_evidence_per_report = 12
max_search_results = 4

[graph.global_search.drift]
enabled = false
max_subqueries = 4

[rerank]
# Endpoint/model settings point at the optional reranker service.
enabled = false
# Non-local rerank endpoints require explicit document-export consent.
allow_document_export = false
# strategy selects endpoint or local LLM reranking.
strategy = "endpoint"          # endpoint | llm
# Keep provider/base_url/model aligned with the rerank endpoint you run.
provider = "vllm"              # vllm | cohere | jina
base_url = "http://127.0.0.1:8003"
model = "Qwen/Qwen3-Reranker-4B"
top_n = 12
# Timeout for each rerank request.
timeout_seconds = 120
# Leave api_key empty unless the reranker requires auth.
api_key = ""
capability_cache_ttl_seconds = 60
# concurrency and queue timeout bound endpoint backpressure.
max_concurrent_requests = 4
queue_timeout_seconds = 300

[rerank.retry]
# Retry/backoff applies to the rerank endpoint.
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[context]
enabled = true

[vision]
# Enable visual captioning/VQA only if you ingest or query images.
enabled = false
# Keep provider/base_url/model aligned with the vision endpoint you run.
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
# Timeout for each vision request.
timeout_seconds = 180
# Leave api_key empty unless the vision service requires auth.
api_key = ""
# concurrency and queue timeout bound endpoint backpressure.
max_concurrent_requests = 4
queue_timeout_seconds = 300

[vision.retry]
# Retry/backoff applies to the vision endpoint.
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[ocr]
# Enable OCR for scanned/image-only PDFs when text extraction is unavailable.
enabled = false
provider = "external_command"
engine = "external"
language = "eng"
profile = "default"
command = ""
args = []
timeout_seconds = 120
max_stdout_bytes = 4194304
max_stderr_bytes = 65536

[chat]
# Enable generated answers only when you want chat-model synthesis on top of retrieval.
enabled = true
# Keep provider/base_url/model aligned with the chat endpoint you run.
provider = "openai_compatible"
base_url = "http://127.0.0.1:8000/v1"
model = "Qwen/Qwen3.6-27B"
temperature = 0.0
# Timeout for each chat request.
timeout_seconds = 120
# Leave api_key empty unless the chat service requires auth.
api_key = ""
# concurrency and queue timeout bound endpoint backpressure.
max_concurrent_requests = 4
queue_timeout_seconds = 300

[chat.retry]
# Retry/backoff applies to the chat endpoint.
max_retries = 3
initial_backoff_millis = 500
max_backoff_millis = 5000

[chat.vision_attachments]
# Attach images only when the chat model can use vision input.
enabled = false
model_supports_vision = false
max_images = 2
max_total_bytes = 8388608
detail = "auto"

[verifier]
# Citation verification applies to generated answers, not retrieval-only paths.
enabled = true

[qdrant]
# Enable only when you want an external Qdrant vector index for search.
enabled = false
url = "http://rpi4b:6334"
collection = "verbatim"
prefer_for_search = false
timeout_seconds = 5

[index_gc]
retain_previous_generations = 1
stale_staging_seconds = 86400

[cli]
# Caps `verbatim task wait`. Model timeout_seconds values above bound provider
# calls and finite daemon HTTP requests; they do not cap task wait streams.
task_wait_timeout_seconds = 1500

[daemon]
# HTTP bind address for the daemon process.
bind = "127.0.0.1:7700"
# Number of tokio worker threads. Lower values reduce glibc malloc arena
# proliferation (each thread gets its own arena, each up to 128 MB virtual).
# Default 4 is sufficient for typical RAG workloads.
# Set to 0 for num_cpus (tokio default, can cause 1+ GB RSS on many-core machines).
worker_threads = 4

[daemon.auth]
# local-anonymous permits only loopback callers. static-token requires a Bearer token.
mode = "local-anonymous"
# Configure this only for static-token mode. VERBATIM_AUTH_TOKEN overrides it.
static_token = ""
# Static tokens on non-loopback HTTP binds expose credentials. Keep false unless
# the network is explicitly trusted and secured outside this daemon.
allow_insecure_transport = false
# Role granted to callers with the configured static token: reader, editor, or admin.
static_token_role = "admin"

[daemon.idle_reclaim]
# idle reclaim trims memory after idle periods; it can pause briefly when it runs.
# Disabled by default because SQLite shrink and allocator trim can pause briefly.
enabled = false
idle_timeout_seconds = 300
min_interval_seconds = 900
sqlite_shrink_memory = true
malloc_trim = true

[daemon.idle_exit]
# idle exit stops the daemon after inactivity.
# Disabled by default; the daemon remains long-running unless explicitly enabled.
enabled = false
timeout_seconds = 300
# Health checks do not extend the idle deadline unless this is enabled.
count_health_requests = false
# Active collection watcher roots block idle exit by default. Enabling this
# requires startup or first-request watcher maintenance resync to avoid missed
# filesystem events while the daemon was stopped.
allow_with_collection_watcher = false
# Lets explicitly opted-in CLI status calls start verbatim.service and retry once.
# This is not systemd socket activation.
auto_start_on_cli = false

[daemon.resources]
# Memory budget enforcement and poll interval keep RSS within a bounded envelope.
memory_budget_enforcement = "slow_warn"
memory_budget_poll_millis = 500
memory_reservation_margin_percent = 25

[collection_watcher]
# Collection watcher scans roots for filesystem changes and debounces follow-up maintenance.
enabled = true
debounce_millis = 500
max_depth = 32
max_queued_tasks = 128
ignore_collections = []
ignore_paths = []
"#;
