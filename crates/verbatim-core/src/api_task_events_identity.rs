use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::TaskEventsResponse;
use crate::task::{TaskEvent, TaskId};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
struct TaskEventsIdentityBody<'a> {
    task_id: &'a TaskId,
    events: &'a [TaskEvent],
}

fn stamp_task_events_identity(
    task_id: &TaskId,
    events: &[TaskEvent],
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let body = TaskEventsIdentityBody { task_id, events };
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskEvents,
        WIRE_SCHEMA_VERSION,
        task_id.0.clone(),
        &encode_wire_document(&body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("task-events identity does not match the event page response body");
        }
    }
    Ok(expected)
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskEventsResponseWire {
    task_id: TaskId,
    #[serde(default)]
    events: Vec<TaskEvent>,
    identity: CanonicalIdentity,
}

impl TaskEventsResponse {
    pub fn new(task_id: TaskId, events: Vec<TaskEvent>) -> Result<Self> {
        let identity = stamp_task_events_identity(&task_id, &events, None)?;
        Ok(Self {
            task_id,
            events,
            identity,
        })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        stamp_task_events_identity(&self.task_id, &self.events, Some(&self.identity))
    }
}

impl Serialize for TaskEventsResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        TaskEventsResponseWire {
            task_id: self.task_id.clone(),
            events: self.events.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskEventsResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskEventsResponseWire::deserialize(deserializer)?;
        let identity =
            stamp_task_events_identity(&wire.task_id, &wire.events, Some(&wire.identity))
                .map_err(serde::de::Error::custom)?;
        Ok(Self {
            task_id: wire.task_id,
            events: wire.events,
            identity,
        })
    }
}
