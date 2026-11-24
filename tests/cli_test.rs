use assert_cmd::Command;
use predicates::prelude::*;

fn stips() -> Command {
    // Use the CARGO_BIN_EXE_stips env var set by cargo for integration tests.
    Command::new(env!("CARGO_BIN_EXE_stips"))
}

// --help succeeds and mentions the binary name and subcommands.
#[test]
fn test_help() {
    stips()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("stips"))
        .stdout(predicate::str::contains("Usage"));
}

#[test]
fn test_version() {
    stips()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("stips"));
}

#[test]
fn test_override_is_rejected_on_all_read_commands_without_echo() {
    let diagnostic =
        "Error: OPENROUTER_BASE_URL is no longer supported. Unset it to use OpenRouter.\n";
    for args in [
        &[][..],
        &["credits"][..],
        &["credits", "--json"][..],
        &["usage"][..],
        &["usage", "--json"][..],
    ] {
        for endpoint in [
            "",
            "https://openrouter.ai",
            "https://example.invalid/url-sentinel",
        ] {
            stips()
                .args(args)
                .env("OPENROUTER_BASE_URL", endpoint)
                // An invalid header also prevents network I/O if the guard regresses.
                .env("OPENROUTER_API_KEY", "offline-override-key-sentinel\n")
                .assert()
                .code(1)
                .stdout("")
                .stderr(diagnostic);
        }
    }
}

#[cfg(unix)]
#[test]
fn test_non_unicode_override_is_rejected() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    stips()
        .arg("usage")
        .env("OPENROUTER_BASE_URL", OsStr::from_bytes(b"\xff"))
        .env("OPENROUTER_API_KEY", "offline-override-key-sentinel\n")
        .assert()
        .code(1)
        .stdout("")
        .stderr("Error: OPENROUTER_BASE_URL is no longer supported. Unset it to use OpenRouter.\n");
}

#[test]
fn test_retired_key_save_rejects_trailing_args_without_echo() {
    let sentinel = "offline-retired-key-sentinel";
    let diagnostic = "Error: This credential command has been retired. Set OPENROUTER_API_KEY or use macOS Keychain Access (service: openrouter-api-key, account: openrouter).\n";
    for trailing in [
        &[][..],
        &[sentinel][..],
        &[sentinel, "--help", "--unknown"][..],
    ] {
        stips()
            .args(["key", "save"])
            .args(trailing)
            // Never allow this test to reach the system security tool.
            .env("PATH", "")
            .assert()
            .code(1)
            .stdout("")
            .stderr(diagnostic);
    }
}

#[test]
fn test_key_help_lists_open_and_omits_save() {
    stips()
        .args(["key", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("save").not());
}

// `key` with no subcommand: clap prints a usage block and exits 2.
// The stderr contains "COMMAND" as part of the usage line.
#[test]
fn test_key_no_subcommand() {
    stips()
        .arg("key")
        .assert()
        .failure()
        .stderr(predicate::str::contains("COMMAND"));
}

// Completely unknown top-level subcommand is rejected by clap.
#[test]
fn test_unknown_subcommand() {
    stips()
        .arg("bogus")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}
