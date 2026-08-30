/// Shared identities, roles, and daemon authentication configuration.
pub mod auth;
pub use auth::{AuthMode, DaemonAuthConfig, Principal, Role};
pub mod adk_integration;
pub mod api;
#[path = "api_collection_list_result_identity.rs"]
pub mod collection_list_result_identity;
pub use collection_list_result_identity::CollectionListResponse;
#[path = "api_deletion_report_list_result_identity.rs"]
pub mod deletion_report_list_result_identity;
pub use deletion_report_list_result_identity::DeletionReportListResponse;
#[path = "api_source_list_result_identity.rs"]
pub mod source_list_result_identity;
pub use source_list_result_identity::SourceListResponse;
#[path = "api_deletion_report_result_identity.rs"]
pub mod deletion_report_result_identity;
pub use deletion_report_result_identity::DeletionReportResponse;
#[path = "api_ask_retrieval_debug_identity.rs"]
mod api_ask_retrieval_debug_identity;
pub mod cache_identity;
pub mod canonical_chunker;
#[cfg(test)]
#[path = "api_deletion_report_result_identity_wire_tests.rs"]
mod deletion_report_result_identity_wire_tests;

pub mod chunker;
pub mod citation_audit;
pub mod collection;
pub mod compare_sources;
pub mod config;
pub mod context;
pub mod deletion;
pub mod diskann3;
pub mod diskann3_backend;
pub mod diskann3_service;
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
pub mod lancedb_backend;
pub mod legacy_vector_cutover;
pub mod lexical_engine;
pub mod memory_budget;
pub mod migration_framework;
pub mod multi_hop_research;
pub mod named_vector_spaces;
pub mod observability_contract;
pub mod ocr;
pub mod overfetch;
pub mod page_layout;
pub mod pagination;
pub mod parser;
pub mod pdf_selector;
pub mod profiles;
pub mod provider;
pub mod qdrant_backend;
pub mod remote_storage_client;
pub mod resource;
pub mod result_diversity;
pub mod retrieval_budgets;
pub mod retrieval_telemetry;
pub mod retrieve;
pub mod sdk;
pub mod search_planner;
mod source_bounded_output;
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
mod diskann3_service_tests;

#[cfg(test)]
mod lancedb_backend_tests;

#[cfg(test)]
mod qdrant_backend_tests;

#[cfg(test)]
#[path = "retrieval_telemetry_tests.rs"]
mod retrieval_telemetry_tests;

#[cfg(test)]
#[path = "ssd_vector_benchmark_tests.rs"]
mod ssd_vector_benchmark_tests;

#[cfg(test)]
mod named_vector_spaces_tests;

pub mod wire_schemas;
