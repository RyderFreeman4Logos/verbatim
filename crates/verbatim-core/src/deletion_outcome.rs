use anyhow::{bail, Result};

use crate::deletion::DeletionOutcome;

pub(super) fn outcome_to_str(outcome: DeletionOutcome) -> &'static str {
    match outcome {
        DeletionOutcome::Erased => "erased",
        DeletionOutcome::Pending => "pending",
        DeletionOutcome::Held => "held",
        DeletionOutcome::NotFound => "not_found",
    }
}

pub(super) fn outcome_from_str(outcome: &str) -> Result<DeletionOutcome> {
    match outcome {
        "erased" => Ok(DeletionOutcome::Erased),
        "pending" => Ok(DeletionOutcome::Pending),
        "held" => Ok(DeletionOutcome::Held),
        "not_found" => Ok(DeletionOutcome::NotFound),
        _ => bail!("unknown deletion outcome: {outcome}"),
    }
}
