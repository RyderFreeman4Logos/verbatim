//! Report-artifact CLI lookup through the daemon client.

use std::io::Write;

use crate::client::{write_report_artifact, CliError, DaemonClient};

pub(super) fn run<W, C>(id: &str, stdout: &mut W, client: &C) -> Result<u8, CliError>
where
    W: Write,
    C: DaemonClient,
{
    write_report_artifact(stdout, &client.get_report_artifact(id)?)?;
    Ok(0)
}
