use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use verbatim_core::config::{self, Config};

use crate::client::{CliError, CliResult};

pub trait LocalActions {
    fn config_init(&self) -> CliResult<PathBuf>;
    fn config_validate(&self) -> CliResult<PathBuf>;
    fn daemon_start(&self) -> CliResult<u8>;
    fn daemon_install(&self) -> CliResult<PathBuf>;
}

#[derive(Default)]
pub struct RealLocalActions;

impl LocalActions for RealLocalActions {
    fn config_init(&self) -> CliResult<PathBuf> {
        config::init_default_config()
            .map_err(|error| CliError::Api(format!("failed to initialize config: {error:#}")))
    }

    fn config_validate(&self) -> CliResult<PathBuf> {
        let path = config::config_path();
        Config::load_from(&path)
            .map_err(|error| CliError::Api(format!("config is invalid: {error:#}")))?;
        Ok(path)
    }

    fn daemon_start(&self) -> CliResult<u8> {
        let daemon =
            std::env::var("VERBATIM_DAEMON_BIN").unwrap_or_else(|_| "verbatim-daemon".into());
        let status = Command::new(&daemon).status().map_err(|error| {
            CliError::Api(format!("failed to start {daemon} in foreground: {error}"))
        })?;
        Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
    }

    fn daemon_install(&self) -> CliResult<PathBuf> {
        let unit_path = user_systemd_unit_path()?;
        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let daemon =
            std::env::var("VERBATIM_DAEMON_BIN").unwrap_or_else(|_| "verbatim-daemon".into());
        fs::write(&unit_path, systemd_unit(&daemon))?;
        Ok(unit_path)
    }
}

pub fn write_config_init<W>(writer: &mut W, path: &std::path::Path) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Wrote config: {}", path.display())
}

pub fn write_config_validate<W>(writer: &mut W, path: &std::path::Path) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Config valid: {}", path.display())
}

pub fn write_daemon_install<W>(writer: &mut W, path: &std::path::Path) -> std::io::Result<()>
where
    W: Write,
{
    writeln!(writer, "Wrote systemd user unit: {}", path.display())?;
    writeln!(writer, "Reload with: systemctl --user daemon-reload")?;
    writeln!(writer, "Start with: systemctl --user start verbatim")
}

fn user_systemd_unit_path() -> CliResult<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| CliError::Api("HOME or XDG_CONFIG_HOME is required".into()))?;
    Ok(config_home
        .join("systemd")
        .join("user")
        .join("verbatim.service"))
}

fn systemd_unit(daemon: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Verbatim daemon\n\
         After=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={daemon}\n\
         Restart=on-failure\n\
         RestartSec=2\n\n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_uses_daemon_entrypoint() {
        let unit = systemd_unit("/bin/verbatim-daemon");

        assert!(unit.contains("ExecStart=/bin/verbatim-daemon"));
        assert!(unit.contains("Restart=on-failure"));
    }
}
