//! Config-init template tests relocated from the parent crate root.
//!
//! These verify that `verbatim init` writes the full expected default config
//! template, including the SQLite store durability profile.

use super::*;

#[test]
fn config_init_template_documents_store_and_parser() {
    let template = generated_config_template();

    assert!(template.contains("[store]"));
    assert!(template.contains("SQLite metadata/text"));
    assert!(template.contains("durability = \"balanced\""));
    assert!(template.contains("vectors"));
    assert!(template.contains("BM25"));
    assert!(template.contains("[parser]"));
    assert!(template.contains("born-digital"));
    assert!(template.contains("scanned/image-only"));
    assert!(template.contains("image artifact"));
}

#[test]
fn config_init_template_documents_model_and_retrieval_knobs() {
    let _env = EnvGuard::capture(&["OPENAI_API_KEY"]);
    std::env::set_var("OPENAI_API_KEY", "sentinel-secret-value");

    let template = generated_config_template();

    assert!(template.contains("[embedding]"));
    assert!(template.contains("[rerank]"));
    assert!(template.contains("endpoint"));
    assert!(template.contains("model"));
    assert!(template.contains("concurrency"));
    assert!(template.contains("timeout"));
    assert!(template.contains("[retrieval]"));
    assert!(template.contains("RRF fusion"));
    assert!(template.contains("page size"));
    assert!(template.contains("OCR"));
    assert!(template.contains("vision"));
    assert!(template.contains("qdrant"));
    assert!(template.contains("api_key = \"\""));
    assert!(!template.contains("sentinel-secret-value"));
}

#[test]
fn config_init_template_documents_daemon_resources_and_watcher() {
    let template = generated_config_template();

    assert!(template.contains("[daemon]"));
    assert!(template.contains("bind"));
    assert!(template.contains("worker threads"));
    assert!(template.contains("idle reclaim"));
    assert!(template.contains("idle exit"));
    assert!(template.contains("[daemon.resources]"));
    assert!(template.contains("resources"));
    assert!(template.contains("collection watcher"));
}
