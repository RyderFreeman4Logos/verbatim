use std::env;
use std::process::ExitCode;

const SUPPORTED_COMMANDS: &[&str] = &["source", "ingest", "ask", "evidence", "config", "daemon"];

fn main() -> ExitCode {
    let mut stderr = std::io::stderr();
    match run(env::args().skip(1), &mut std::io::stdout(), &mut stderr) {
        Ok(code) => ExitCode::from(code),
        Err(code) => ExitCode::from(code),
    }
}

fn run<I, W, E>(args: I, stdout: &mut W, stderr: &mut E) -> Result<u8, u8>
where
    I: IntoIterator,
    I::Item: Into<String>,
    W: std::io::Write,
    E: std::io::Write,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            write_help(stdout).map_err(|_| 1)?;
            Ok(0)
        }
        Some("-V" | "--version") => {
            writeln!(stdout, "verbatim {}", env!("CARGO_PKG_VERSION")).map_err(|_| 1)?;
            Ok(0)
        }
        Some(command) if SUPPORTED_COMMANDS.contains(&command) => {
            writeln!(
                stderr,
                "verbatim {command}: CLI thin-client command is not implemented in this MVP. Use verbatim-daemon's REST API directly until issue #14 implements the thin client."
            )
            .map_err(|_| 1)?;
            Err(2)
        }
        Some(command) => {
            writeln!(stderr, "unknown verbatim command: {command}").map_err(|_| 1)?;
            write_help(stderr).map_err(|_| 1)?;
            Err(2)
        }
    }
}

fn write_help<W>(writer: &mut W) -> std::io::Result<()>
where
    W: std::io::Write,
{
    writeln!(
        writer,
        "verbatim {}\n\nUSAGE:\n    verbatim <COMMAND>\n\nCOMMANDS:\n    source     Manage sources (thin client pending #14)\n    ingest     Trigger ingestion (thin client pending #14)\n    ask        Ask the daemon (thin client pending #14)\n    evidence   Inspect evidence (thin client pending #14)\n    config     Inspect or update config (thin client pending #14)\n    daemon     Manage daemon process/API (thin client pending #14)\n\nOPTIONS:\n    -h, --help       Print help\n    -V, --version    Print version\n\nThe installable CLI intentionally fails explicitly for command invocations until the thin daemon client is implemented."
        ,
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_prints_package_version() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["--version"], &mut stdout, &mut stderr).unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("verbatim {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn documented_commands_fail_explicitly_until_thin_client_exists() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["ask", "What is freedom?"], &mut stdout, &mut stderr).unwrap_err();

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        let error = String::from_utf8(stderr).unwrap();
        assert!(error.contains("verbatim ask"));
        assert!(error.contains("not implemented"));
        assert!(error.contains("#14"));
    }

    #[test]
    fn unknown_commands_fail_instead_of_being_ignored() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run(["unknown"], &mut stdout, &mut stderr).unwrap_err();

        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        let error = String::from_utf8(stderr).unwrap();
        assert!(error.contains("unknown verbatim command: unknown"));
        assert!(error.contains("USAGE:"));
    }
}
