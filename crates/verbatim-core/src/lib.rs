/// Shared identities, roles, and daemon authentication configuration.
pub mod auth;
pub use auth::{AuthMode, DaemonAuthConfig, Principal, Role};
pub mod adk_integration;
pub mod api;
pub mod cache_identity;
pub mod canonical_chunker;
mod caption_chunker;
pub mod chunker;
pub mod citation_audit;
pub mod collection;
pub mod compare_sources;
pub mod config;
pub mod context;
pub mod deletion;
pub mod diskann3;
pub mod diskann3_backend;
pub mod durability;
pub mod durable_updates;
pub mod embed;
pub mod enterprise_predicates;
pub mod erasure;
#[path = "types_evidence_spans.rs"]
pub mod evidence_spans;
pub mod exact_scan;
pub mod exhaustive_audit;
pub mod generate;
pub mod generation_publication;
pub mod graph_extraction;
pub mod graphrag;
pub mod grounded_answer;
pub mod hybrid_fusion;
pub mod image_limits;
pub mod index;
pub mod index_gc;
pub mod index_profile_delete;
pub mod index_publication;
pub mod ingest;
pub mod ingest_security;
pub mod lexical_engine;
pub mod memory_budget;
pub mod migration_framework;
pub mod multi_hop_research;
pub mod observability_contract;
pub mod ocr;
pub mod overfetch;
pub mod page_layout;
pub mod pagination;
pub mod parser;
pub mod profiles;
pub mod provider;
pub mod remote_storage_client;
pub mod resource;
pub mod result_diversity;
pub mod retrieval_budgets;
pub mod retrieval_telemetry;
pub mod retrieve;
pub mod sdk;
pub mod search_planner;
pub mod source_metadata;
pub mod ssd_vector_benchmark;
pub mod storage_ports;
pub mod store;
pub mod task;
pub mod traits;
pub mod types;
pub mod upstream;
pub mod vector_shards;
pub mod vision_caption;

#[cfg(test)]
#[path = "diskann3_backend/tests.rs"]
mod diskann3_backend_tests;

#[cfg(test)]
#[path = "retrieval_telemetry_tests.rs"]
mod retrieval_telemetry_tests;

#[cfg(test)]
#[path = "ssd_vector_benchmark_tests.rs"]
mod ssd_vector_benchmark_tests;

pub mod wire_schemas;
