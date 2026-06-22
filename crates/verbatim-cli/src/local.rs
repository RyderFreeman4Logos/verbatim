use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use verbatim_core::config::{self, Config};

use crate::client::{CliError, CliResult};

pub trait LocalActions {
    fn config_init(&self) -> CliResult<PathBuf>;
    fn config_validate(&self) -> CliResult<PathBuf>;
    fn daemon_start(&self) -> CliResult<u8>;
    fn daemon_install(&self, force: bool) -> CliResult<PathBuf>;
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

    fn daemon_install(&self, force: bool) -> CliResult<PathBuf> {
        let unit_path = user_systemd_unit_path()?;
        if unit_path.exists() && !force {
            return Err(existing_unit_error(&unit_path));
        }

        let daemon = find_daemon_binary("verbatim-daemon").ok_or_else(|| {
            CliError::Api(
                "verbatim-daemon was not found on PATH; install it with `just install` \
                 or put verbatim-daemon on PATH before running `verbatim daemon install`"
                    .into(),
            )
        })?;

        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_unit_file(&unit_path, &systemd_unit(&daemon), force)?;
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
    writeln!(writer, "Generated {}", path.display())?;
    writeln!(writer, "Run: systemctl --user daemon-reload")?;
    writeln!(writer, "Run: systemctl --user enable --now verbatim")
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

fn systemd_unit(daemon: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Verbatim RAG daemon\n\
         After=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         Environment=VERBATIM_CONFIG=%h/.config/verbatim/config.toml\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        daemon.display()
    )
}

fn find_daemon_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|path| find_binary_on_path(name, &path))
        .and_then(|path| fs::canonicalize(path).ok())
}

fn find_binary_on_path(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| {
            if dir.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                dir.join(name)
            }
        })
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

fn write_unit_file(unit_path: &Path, contents: &str, force: bool) -> CliResult<()> {
    write_unit_file_with_pre_publish(unit_path, contents, force, || Ok(()))
}

fn write_unit_file_with_pre_publish<F>(
    unit_path: &Path,
    contents: &str,
    force: bool,
    pre_publish: F,
) -> CliResult<()>
where
    F: FnOnce() -> std::io::Result<()>,
{
    let temp_path = temporary_unit_path(unit_path);
    if let Err(error) = write_new_file(&temp_path, contents) {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }

    if let Err(error) = pre_publish() {
        let _ = fs::remove_file(&temp_path);
        return Err(error.into());
    }

    let publish_result = if force {
        replace_unit_file(&temp_path, unit_path)
    } else {
        publish_unit_file_without_overwrite(&temp_path, unit_path)
    };

    if let Err(error) = publish_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if !force {
        let _ = fs::remove_file(&temp_path);
    }
    Ok(())
}

fn write_new_file(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

fn replace_unit_file(temp_path: &Path, unit_path: &Path) -> CliResult<()> {
    fs::rename(temp_path, unit_path).map_err(Into::into)
}

fn publish_unit_file_without_overwrite(temp_path: &Path, unit_path: &Path) -> CliResult<()> {
    match fs::hard_link(temp_path, unit_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(existing_unit_error(unit_path))
        }
        Err(error) => Err(error.into()),
    }
}

fn existing_unit_error(unit_path: &Path) -> CliError {
    CliError::Api(format!(
        "{} already exists; rerun with --force to overwrite it",
        unit_path.display()
    ))
}

fn temporary_unit_path(unit_path: &Path) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let file_name = unit_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("verbatim.service");
    unit_path.with_file_name(format!(".{file_name}.{}.{suffix}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn systemd_unit_uses_issue_shape() {
        let unit = systemd_unit(Path::new("/bin/verbatim-daemon"));

        assert_eq!(
            unit,
            "[Unit]\n\
             Description=Verbatim RAG daemon\n\
             After=network-online.target\n\n\
             [Service]\n\
             Type=simple\n\
             ExecStart=/bin/verbatim-daemon\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             Environment=VERBATIM_CONFIG=%h/.config/verbatim/config.toml\n\n\
             [Install]\n\
             WantedBy=default.target\n"
        );
    }

    #[test]
    fn daemon_install_generates_unit_under_xdg_config_home() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::capture(&["XDG_CONFIG_HOME", "HOME", "PATH"]);
        let tempdir = unique_temp_dir("generate");
        let bindir = tempdir.join("bin");
        let daemon = write_fake_daemon(&bindir);
        let config_home = tempdir.join("config");
        env::set_var("XDG_CONFIG_HOME", &config_home);
        env::set_var("PATH", &bindir);

        let path = RealLocalActions.daemon_install(false).unwrap();

        assert_eq!(
            path,
            config_home
                .join("systemd")
                .join("user")
                .join("verbatim.service")
        );
        let unit = fs::read_to_string(&path).unwrap();
        assert!(unit.contains(&format!("ExecStart={}", daemon.display())));
        assert!(unit.contains("Description=Verbatim RAG daemon"));
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("Environment=VERBATIM_CONFIG=%h/.config/verbatim/config.toml"));
        fs::remove_dir_all(tempdir).unwrap();
    }

    #[test]
    fn daemon_install_does_not_overwrite_existing_unit_without_force() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::capture(&["XDG_CONFIG_HOME", "HOME", "PATH"]);
        let tempdir = unique_temp_dir("no-overwrite");
        let bindir = tempdir.join("bin");
        write_fake_daemon(&bindir);
        let config_home = tempdir.join("config");
        let unit_path = config_home
            .join("systemd")
            .join("user")
            .join("verbatim.service");
        fs::create_dir_all(unit_path.parent().unwrap()).unwrap();
        fs::write(&unit_path, "existing").unwrap();
        env::set_var("XDG_CONFIG_HOME", &config_home);
        env::set_var("PATH", &bindir);

        let error = RealLocalActions.daemon_install(false).unwrap_err();

        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("--force"));
        assert_eq!(fs::read_to_string(&unit_path).unwrap(), "existing");
        fs::remove_dir_all(tempdir).unwrap();
    }

    #[test]
    fn daemon_install_does_not_clobber_unit_created_during_non_force_publish() {
        let tempdir = unique_temp_dir("publish-race");
        let unit_path = tempdir.join("verbatim.service");

        let error = write_unit_file_with_pre_publish(&unit_path, "new", false, || {
            fs::write(&unit_path, "concurrent")
        })
        .unwrap_err();

        assert_eq!(error.exit_code(), 1);
        assert!(error.to_string().contains("--force"));
        assert_eq!(fs::read_to_string(&unit_path).unwrap(), "concurrent");
        fs::remove_dir_all(tempdir).unwrap();
    }

    #[test]
    fn daemon_install_force_overwrites_existing_unit() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::capture(&["XDG_CONFIG_HOME", "HOME", "PATH"]);
        let tempdir = unique_temp_dir("force");
        let bindir = tempdir.join("bin");
        let daemon = write_fake_daemon(&bindir);
        let config_home = tempdir.join("config");
        let unit_path = config_home
            .join("systemd")
            .join("user")
            .join("verbatim.service");
        fs::create_dir_all(unit_path.parent().unwrap()).unwrap();
        fs::write(&unit_path, "existing").unwrap();
        env::set_var("XDG_CONFIG_HOME", &config_home);
        env::set_var("PATH", &bindir);

        let path = RealLocalActions.daemon_install(true).unwrap();

        assert_eq!(path, unit_path);
        let unit = fs::read_to_string(&path).unwrap();
        assert_ne!(unit, "existing");
        assert!(unit.contains(&format!("ExecStart={}", daemon.display())));
        fs::remove_dir_all(tempdir).unwrap();
    }

    #[test]
    fn daemon_install_missing_binary_fails_without_writing_unit() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let _env = EnvGuard::capture(&["XDG_CONFIG_HOME", "HOME", "PATH"]);
        let tempdir = unique_temp_dir("missing-binary");
        let config_home = tempdir.join("config");
        let empty_path = tempdir.join("empty-path");
        fs::create_dir_all(&empty_path).unwrap();
        env::set_var("XDG_CONFIG_HOME", &config_home);
        env::set_var("PATH", &empty_path);

        let error = RealLocalActions.daemon_install(false).unwrap_err();

        assert_eq!(error.exit_code(), 1);
        assert!(error
            .to_string()
            .contains("verbatim-daemon was not found on PATH"));
        assert!(error.to_string().contains("just install"));
        assert!(!config_home
            .join("systemd")
            .join("user")
            .join("verbatim.service")
            .exists());
        fs::remove_dir_all(tempdir).unwrap();
    }

    struct EnvGuard {
        values: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn capture(names: &[&'static str]) -> Self {
            let values = names
                .iter()
                .map(|name| (*name, env::var_os(name)))
                .collect();
            Self { values }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.values {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "verbatim-cli-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_fake_daemon(bindir: &Path) -> PathBuf {
        fs::create_dir_all(bindir).unwrap();
        let path = bindir.join("verbatim-daemon");
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).unwrap();
        }

        path
    }
}
