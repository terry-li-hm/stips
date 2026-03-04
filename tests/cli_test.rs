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

// --version is not wired up in the Cli struct (no `#[command(version)]`),
// so clap rejects it as an unknown argument.  This test pins that behaviour:
// unknown flag → exit failure, stderr contains "error".
#[test]
fn test_version() {
    stips()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("stips"));
}

// No args: stips defaults to the credits subcommand, which requires a
// keychain entry.  Without one the binary exits 1 and prints missing-key
// guidance on stderr.
// Marked #[ignore] because the test machine may have a real keychain entry
// that would trigger an outbound network call instead of the error path.
#[test]
#[ignore = "requires no keychain entry present; would make a network call if one exists"]
fn test_no_args_missing_key() {
    stips()
        .assert()
        .failure()
        .stderr(predicate::str::contains("stips key save"));
}

// `credits` subcommand also requires a keychain entry; same error path.
// Ignored for the same reason as test_no_args_missing_key.
#[test]
#[ignore = "requires no keychain entry present; would make a network call if one exists"]
fn test_credits_missing_key() {
    stips()
        .arg("credits")
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not found"));
}

// `usage` subcommand also requires a keychain entry.
// Ignored for the same reason as test_no_args_missing_key.
#[test]
#[ignore = "requires no keychain entry present; would make a network call if one exists"]
fn test_usage_missing_key() {
    stips()
        .arg("usage")
        .assert()
        .failure()
        .stderr(predicate::str::contains("API key not found"));
}

// `key save` with no key argument is a clap parse error → exit 2.
#[test]
fn test_key_save_missing_argument() {
    stips()
        .args(["key", "save"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
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
