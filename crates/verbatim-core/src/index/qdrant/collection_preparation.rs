use anyhow::{bail, Result};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::Value;

use super::{QdrantClient, QdrantEnvelope, DISTANCE};

const PAYLOAD_INDEX_FIELDS: [&str; 2] = ["profile_id", "source_id"];

impl QdrantClient {
    pub(super) async fn ensure_collection(&self, dimension: usize) -> Result<()> {
        let info = match self.collection_info().await? {
            Some(info) => info,
            None => {
                self.create_collection(dimension).await?;
                let Some(info) = self.collection_info().await? else {
                    bail!("qdrant collection is still missing after creation");
                };
                info
            }
        };
        validate_vector_schema(&info, dimension)?;
        let missing_indexes = missing_payload_indexes(&info)?;
        if missing_indexes.is_empty() {
            return Ok(());
        }
        for field_name in missing_indexes {
            self.create_payload_index(field_name).await?;
        }
        let Some(info) = self.collection_info().await? else {
            bail!("qdrant collection is missing while verifying payload indexes");
        };
        validate_vector_schema(&info, dimension)?;
        let missing_indexes = missing_payload_indexes(&info)?;
        if !missing_indexes.is_empty() {
            bail!(
                "qdrant payload index verification failed: missing {}",
                missing_indexes.join(", ")
            );
        }
        Ok(())
    }

    async fn collection_info(&self) -> Result<Option<Value>> {
        let response = self
            .send_without_body(
                Method::GET,
                &self.collection_path(""),
                "get qdrant collection info",
            )
            .await?;
        if response.response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = self
            .decode_response::<QdrantEnvelope<Value>>(response, "get qdrant collection info")
            .await?;
        Ok(Some(response.result))
    }

    async fn create_payload_index(&self, field_name: &str) -> Result<()> {
        let body = QdrantCreateFieldIndexRequest {
            field_name,
            field_schema: "keyword",
        };
        let _: QdrantEnvelope<Value> = self
            .send_json(
                Method::PUT,
                &self.collection_path("index?wait=true"),
                &body,
                "create qdrant payload index",
            )
            .await?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct QdrantCreateFieldIndexRequest<'a> {
    field_name: &'a str,
    field_schema: &'static str,
}

fn validate_vector_schema(info: &Value, expected_dimension: usize) -> Result<()> {
    let Some(vectors) = info
        .get("config")
        .and_then(|config| config.get("params"))
        .and_then(|params| params.get("vectors"))
    else {
        bail!("qdrant collection schema is malformed: expected result.config.params.vectors");
    };
    let Some(vectors) = vectors.as_object() else {
        bail!("qdrant collection schema is malformed: expected unnamed vector parameters");
    };
    let has_unknown_vector_field = vectors.keys().any(|key| {
        !matches!(
            key.as_str(),
            "size"
                | "distance"
                | "hnsw_config"
                | "quantization_config"
                | "on_disk"
                | "datatype"
                | "multivector_config"
        )
    });
    if has_unknown_vector_field {
        if vectors.contains_key("size") || vectors.contains_key("distance") {
            bail!("qdrant collection schema is ambiguous: found unnamed and named vectors");
        }
        bail!("qdrant collection schema uses named vectors; expected one unnamed vector");
    }
    let Some(actual_dimension) = vectors.get("size").and_then(Value::as_u64) else {
        bail!("qdrant collection schema is malformed: expected integer vector dimension");
    };
    let Ok(actual_dimension) = usize::try_from(actual_dimension) else {
        bail!("qdrant collection schema has unsupported vector dimension {actual_dimension}");
    };
    if actual_dimension != expected_dimension {
        bail!(
            "qdrant collection schema mismatch: expected dimension {expected_dimension}, actual {actual_dimension}"
        );
    }
    let Some(actual_distance) = vectors.get("distance").and_then(Value::as_str) else {
        bail!("qdrant collection schema is malformed: expected distance string");
    };
    if actual_distance != DISTANCE {
        bail!(
            "qdrant collection schema mismatch: expected distance {DISTANCE}, actual {actual_distance}"
        );
    }
    Ok(())
}

fn missing_payload_indexes(info: &Value) -> Result<Vec<&'static str>> {
    let Some(payload_schema) = info.get("payload_schema").and_then(Value::as_object) else {
        bail!("qdrant collection schema is malformed: expected result.payload_schema object");
    };
    let mut missing = Vec::new();
    for field_name in PAYLOAD_INDEX_FIELDS {
        let Some(index_info) = payload_schema.get(field_name) else {
            missing.push(field_name);
            continue;
        };
        let Some(data_type) = index_info.get("data_type").and_then(Value::as_str) else {
            bail!(
                "qdrant payload index {field_name} is malformed: expected data_type string, actual {index_info}"
            );
        };
        if data_type != "keyword" {
            bail!(
                "qdrant payload index {field_name} mismatch: expected keyword, actual {data_type}"
            );
        }
    }
    Ok(missing)
}
