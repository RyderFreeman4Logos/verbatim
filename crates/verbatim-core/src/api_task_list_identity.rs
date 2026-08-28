use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{TaskListAggregate, TaskListResponse, TaskSummary};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
struct TaskListIdentityBody<'a> {
    tasks: &'a [TaskSummary],
    total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    aggregate: Option<&'a TaskListAggregate>,
}

fn stamp_task_list_identity(
    tasks: &[TaskSummary],
    total: usize,
    aggregate: Option<&TaskListAggregate>,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let body = TaskListIdentityBody {
        tasks,
        total,
        aggregate,
    };
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskList,
        WIRE_SCHEMA_VERSION,
        "task-list",
        &encode_wire_document(&body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("task-list identity does not match the task list response body");
        }
    }
    Ok(expected)
}

#[derive(Debug, Serialize, Deserialize)]
struct TaskListResponseWire {
    #[serde(default)]
    tasks: Vec<TaskSummary>,
    total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aggregate: Option<TaskListAggregate>,
    identity: CanonicalIdentity,
}

impl TaskListResponse {
    pub fn new(
        tasks: Vec<TaskSummary>,
        total: usize,
        aggregate: Option<TaskListAggregate>,
    ) -> Result<Self> {
        let identity = stamp_task_list_identity(&tasks, total, aggregate.as_ref(), None)?;
        Ok(Self {
            tasks,
            total,
            aggregate,
            identity,
        })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        stamp_task_list_identity(
            &self.tasks,
            self.total,
            self.aggregate.as_ref(),
            Some(&self.identity),
        )
    }
}

impl Serialize for TaskListResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        TaskListResponseWire {
            tasks: self.tasks.clone(),
            total: self.total,
            aggregate: self.aggregate.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskListResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskListResponseWire::deserialize(deserializer)?;
        let identity = stamp_task_list_identity(
            &wire.tasks,
            wire.total,
            wire.aggregate.as_ref(),
            Some(&wire.identity),
        )
        .map_err(serde::de::Error::custom)?;
        Ok(Self {
            tasks: wire.tasks,
            total: wire.total,
            aggregate: wire.aggregate,
            identity,
        })
    }
}
