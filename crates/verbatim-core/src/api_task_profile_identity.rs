use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{TaskProfile, TaskProfileResponse};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize, Deserialize)]
struct TaskProfileResponseWire {
    profile: TaskProfile,
    identity: CanonicalIdentity,
}

fn stamp_task_profile_identity(
    profile: &TaskProfile,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::TaskProfile,
        WIRE_SCHEMA_VERSION,
        profile.task_id.0.clone(),
        &encode_wire_document(profile)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("task-profile identity does not match the task profile response body");
        }
    }
    Ok(expected)
}

impl TaskProfileResponse {
    pub fn new(profile: TaskProfile) -> Result<Self> {
        let identity = stamp_task_profile_identity(&profile, None)?;
        Ok(Self { profile, identity })
    }
}

impl Serialize for TaskProfileResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = stamp_task_profile_identity(&self.profile, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        TaskProfileResponseWire {
            profile: self.profile.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskProfileResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = TaskProfileResponseWire::deserialize(deserializer)?;
        let identity = stamp_task_profile_identity(&wire.profile, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            profile: wire.profile,
            identity,
        })
    }
}
