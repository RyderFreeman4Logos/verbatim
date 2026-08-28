use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{TaskSummary, TaskSummaryResponse};
use crate::task::TaskSpan;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
struct TaskRunIdentityBody<'a> {
    task: &'a TaskSummary,
    spans: &'a [TaskSpan],
}

impl<'a> TaskRunIdentityBody<'a> {
    fn from_response(response: &'a TaskSummaryResponse) -> Self {
        Self {
            task: &response.task,
            spans: &response.spans,
        }
    }
}

fn stamp_task_run_identity(
    body: &TaskRunIdentityBody<'_>,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskRun,
        WIRE_SCHEMA_VERSION,
        body.task.id.0.clone(),
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("task-run identity does not match the task summary response body");
        }
    }
    Ok(expected)
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskSummaryResponseWire {
    task: TaskSummary,
    #[serde(default)]
    spans: Vec<TaskSpan>,
    identity: CanonicalIdentity,
}

impl TaskSummaryResponse {
    pub fn new(task: TaskSummary, spans: Vec<TaskSpan>) -> Result<Self> {
        let body = TaskRunIdentityBody {
            task: &task,
            spans: &spans,
        };
        let identity = stamp_task_run_identity(&body, None)?;
        Ok(Self {
            task,
            spans,
            identity,
        })
    }

    fn identity_body(&self) -> TaskRunIdentityBody<'_> {
        TaskRunIdentityBody::from_response(self)
    }
}

impl Serialize for TaskSummaryResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = stamp_task_run_identity(&self.identity_body(), Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        TaskSummaryResponseWire {
            task: self.task.clone(),
            spans: self.spans.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskSummaryResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskSummaryResponseWire::deserialize(deserializer)?;
        let body = TaskRunIdentityBody {
            task: &wire.task,
            spans: &wire.spans,
        };
        let identity = stamp_task_run_identity(&body, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            task: wire.task,
            spans: wire.spans,
            identity,
        })
    }
}
