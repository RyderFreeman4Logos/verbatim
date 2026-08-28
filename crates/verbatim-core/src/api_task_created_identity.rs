use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::TaskCreatedResponse;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct TaskCreatedResponseBody<'a> {
    task_id: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskCreatedResponseWire {
    task_id: String,
    identity: CanonicalIdentity,
}

fn stamp_task_created_identity(
    task_id: &str,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskCreated,
        WIRE_SCHEMA_VERSION,
        task_id,
        &encode_wire_document(&TaskCreatedResponseBody { task_id })?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("task-created identity does not match the task-created response body");
        }
    }
    Ok(expected)
}

impl TaskCreatedResponse {
    pub fn new(task_id: impl Into<String>) -> Result<Self> {
        let task_id = task_id.into();
        let identity = stamp_task_created_identity(&task_id, None)?;
        Ok(Self { task_id, identity })
    }
}

impl Serialize for TaskCreatedResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = stamp_task_created_identity(&self.task_id, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        TaskCreatedResponseWire {
            task_id: self.task_id.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskCreatedResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskCreatedResponseWire::deserialize(deserializer)?;
        let identity = stamp_task_created_identity(&wire.task_id, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            task_id: wire.task_id,
            identity,
        })
    }
}
