use super::*;

#[test]
fn issue_332_cli_relocate_parses_and_calls_daemon() {
    let (code, stdout, stderr, client, _) =
        run_mock(["source", "relocate", "src-1", "/srv/verbatim/renamed.md"]);

    assert_eq!(code.unwrap(), 0);
    assert!(stderr.is_empty());
    assert_eq!(
        client.calls.into_inner(),
        ["relocate_source:src-1:/srv/verbatim/renamed.md"]
    );
    assert!(stdout.contains("Relocated source: src-1"));

    let (code, help, stderr, _, _) = run_mock(["source", "relocate", "--help"]);
    assert_eq!(code.unwrap(), 0);
    assert!(stderr.is_empty());
    assert!(help.contains("visible to the daemon host"));
    assert!(help.contains("content must be unchanged"));
}
