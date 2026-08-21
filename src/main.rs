use std::fmt;
use std::io::{self, IsTerminal};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use serde::Deserialize;

const OPENROUTER_KEYS_PAGE: &str = "https://openrouter.ai/keys";
const OPENROUTER_TOPUP_PAGE: &str = "https://openrouter.ai/credits";
const KEYCHAIN_SERVICE: &str = "openrouter-api-key";
const KEYCHAIN_ACCOUNT: &str = "openrouter";
const LOW_BALANCE_THRESHOLD: f64 = 5.0;

#[derive(Parser, Debug)]
#[command(name = "stips", about = "OpenRouter credits and usage CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Credits {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    Usage {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },
}

#[derive(Subcommand, Debug)]
enum KeyCommands {
    Open,
    Save { key: String },
}

#[derive(Debug)]
enum AppError {
    ApiKeyMissing,
    Message(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKeyMissing => {
                write!(
                    f,
                    "Error: API key not found. Run: stips key save <your-key>"
                )
            }
            Self::Message(msg) => write!(f, "{msg}"),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreditsEnvelope {
    data: CreditsData,
}

#[derive(Debug, Deserialize, PartialEq)]
struct CreditsData {
    total_credits: f64,
    total_usage: f64,
}

#[derive(Debug, Deserialize, PartialEq)]
struct UsageEnvelope {
    data: UsageData,
}

#[derive(Debug, Deserialize, PartialEq)]
struct UsageData {
    usage_daily: f64,
    usage_weekly: f64,
    usage_monthly: f64,
}

#[derive(Debug, serde::Serialize, PartialEq)]
struct CreditsOutput {
    remaining: f64,
    used: f64,
    total: f64,
}

#[derive(Debug, serde::Serialize, PartialEq)]
struct UsageOutput {
    daily: f64,
    weekly: f64,
    monthly: f64,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    let is_tty = io::stdout().is_terminal();

    match cli.command {
        None => cmd_credits(is_tty, false),
        Some(Commands::Credits { json }) => cmd_credits(is_tty, json),
        Some(Commands::Usage { json }) => cmd_usage(json),
        Some(Commands::Key { command }) => match command {
            KeyCommands::Open => cmd_key_open(),
            KeyCommands::Save { key } => cmd_key_save(&key),
        },
    }
}

fn cmd_credits(is_tty: bool, json: bool) -> Result<(), AppError> {
    let key = read_api_key_from_keychain()?;
    let url = credits_endpoint(&base_url());
    let response = request_json::<CreditsEnvelope>(&url, &key)?;
    let remaining = remaining_credits(response.data.total_credits, response.data.total_usage);

    if json {
        println!(
            "{}",
            format_credits_json(
                remaining,
                response.data.total_usage,
                response.data.total_credits
            )
            .map_err(|e| AppError::Message(format!("Error: {e}")))?
        );
    } else {
        println!(
            "{}",
            format_credits_text(
                remaining,
                response.data.total_usage,
                response.data.total_credits
            )
        );
        if let Some(msg) = low_balance_message(remaining, is_tty) {
            eprintln!("{msg}");
        }
    }

    Ok(())
}

fn cmd_usage(json: bool) -> Result<(), AppError> {
    let key = read_api_key_from_keychain()?;
    let url = usage_endpoint(&base_url());
    let response = request_json::<UsageEnvelope>(&url, &key)?;

    let daily = normalize_usage(response.data.usage_daily);
    let weekly = normalize_usage(response.data.usage_weekly);
    let monthly = normalize_usage(response.data.usage_monthly);

    if json {
        println!(
            "{}",
            format_usage_json(daily, weekly, monthly)
                .map_err(|e| AppError::Message(format!("Error: {e}")))?
        );
    } else {
        println!("{}", format_usage_text(daily, weekly, monthly));
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn cmd_key_open() -> Result<(), AppError> {
    println!("{OPENROUTER_KEYS_PAGE}");
    Err(AppError::Message(String::from(
        "Error: stips key open requires macOS. Visit the URL above in your browser.",
    )))
}

#[cfg(target_os = "macos")]
fn cmd_key_open() -> Result<(), AppError> {
    println!("Opening {OPENROUTER_KEYS_PAGE}");
    let status = Command::new("open")
        .arg(OPENROUTER_KEYS_PAGE)
        .status()
        .map_err(|err| AppError::Message(format!("Error: failed to run open: {err}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::Message(String::from("Error: failed to open URL")))
    }
}

#[cfg(not(target_os = "macos"))]
fn cmd_key_save(_key: &str) -> Result<(), AppError> {
    Err(AppError::Message(String::from(
        "Error: stips key save requires macOS Keychain. Set OPENROUTER_API_KEY env var instead.",
    )))
}

#[cfg(target_os = "macos")]
fn cmd_key_save(key: &str) -> Result<(), AppError> {
    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
            key,
            "-U",
        ])
        .status()
        .map_err(|err| AppError::Message(format!("Error: failed to run security: {err}")))?;

    if status.success() {
        println!("API key saved to keychain");
        Ok(())
    } else {
        Err(AppError::Message(String::from(
            "Error: failed to save API key to keychain",
        )))
    }
}

fn read_api_key_from_keychain() -> Result<String, AppError> {
    // Check env var first — works on all platforms and enables test injection.
    if let Some(key) = api_key_from_env(std::env::var("OPENROUTER_API_KEY").ok()) {
        return Ok(key);
    }

    #[cfg(not(target_os = "macos"))]
    return Err(AppError::Message(String::from(
        "Error: no OPENROUTER_API_KEY env var set. \
         On macOS, run: stips key save <your-key>",
    )));

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
            .output()
            .map_err(|_| AppError::ApiKeyMissing)?;

        if !output.status.success() {
            return Err(AppError::ApiKeyMissing);
        }

        api_key_from_keychain_stdout(&output.stdout)
    }
}

fn request_json<T: for<'de> Deserialize<'de>>(url: &str, api_key: &str) -> Result<T, AppError> {
    let response = ureq::get(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .call()
        .map_err(|err| AppError::Message(format!("Error: {err}")))?;

    let mut body = response.into_body();
    body.read_json::<T>()
        .map_err(|err| AppError::Message(format!("Error: {err}")))
}

fn base_url() -> String {
    resolve_base_url(std::env::var("OPENROUTER_BASE_URL").ok())
}

/// Returns usage in USD as returned by `GET /api/v1/auth/key`.
/// The API always returns dollar-denominated floats; no unit conversion is needed.
/// Ref: https://openrouter.ai/docs/api/api-reference/overview
fn normalize_usage(value: f64) -> f64 {
    value
}

fn remaining_credits(total_credits: f64, total_usage: f64) -> f64 {
    total_credits - total_usage
}

fn format_credits_text(remaining: f64, used: f64, total: f64) -> String {
    format!("${remaining:.2} remaining  (${used:.2} used of ${total:.2})")
}

fn format_credits_json(remaining: f64, used: f64, total: f64) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&CreditsOutput {
        remaining,
        used,
        total,
    })
}

fn format_usage_text(daily: f64, weekly: f64, monthly: f64) -> String {
    format!("Daily:   ${daily:.2}\nWeekly:  ${weekly:.2}\nMonthly: ${monthly:.2}")
}

fn format_usage_json(daily: f64, weekly: f64, monthly: f64) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&UsageOutput {
        daily,
        weekly,
        monthly,
    })
}

fn low_balance_message(remaining: f64, is_tty: bool) -> Option<String> {
    if remaining < LOW_BALANCE_THRESHOLD {
        if is_tty {
            Some(format!("⚠️  Low — top up at {OPENROUTER_TOPUP_PAGE}"))
        } else {
            Some(format!("Low - top up at {OPENROUTER_TOPUP_PAGE}"))
        }
    } else {
        None
    }
}

fn credits_endpoint(base: &str) -> String {
    format!("{base}/api/v1/credits")
}

fn usage_endpoint(base: &str) -> String {
    format!("{base}/api/v1/auth/key")
}

fn resolve_base_url(from_env: Option<String>) -> String {
    from_env.unwrap_or_else(|| String::from("https://openrouter.ai"))
}

fn api_key_from_env(from_env: Option<String>) -> Option<String> {
    from_env.filter(|key| !key.is_empty())
}

fn api_key_from_keychain_stdout(stdout: &[u8]) -> Result<String, AppError> {
    let key = String::from_utf8_lossy(stdout).trim().to_owned();
    if key.is_empty() {
        Err(AppError::ApiKeyMissing)
    } else {
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_credits(json: &str) -> Result<CreditsEnvelope, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn parse_usage(json: &str) -> Result<UsageEnvelope, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn parse_credits_happy_path_from_docs() {
        let parsed =
            parse_credits(r#"{"data":{"total_credits":100.5,"total_usage":25.75}}"#).unwrap();
        assert_eq!(parsed.data.total_credits, 100.5);
        assert_eq!(parsed.data.total_usage, 25.75);
        assert_eq!(
            remaining_credits(parsed.data.total_credits, parsed.data.total_usage),
            74.75
        );
    }

    #[test]
    fn parse_credits_accepts_integers() {
        let parsed = parse_credits(r#"{"data":{"total_credits":650,"total_usage":636}}"#).unwrap();
        assert_eq!(parsed.data.total_credits, 650.0);
        assert_eq!(parsed.data.total_usage, 636.0);
        assert_eq!(
            remaining_credits(parsed.data.total_credits, parsed.data.total_usage),
            14.0
        );
    }

    #[test]
    fn parse_credits_ignores_unknown_fields() {
        let parsed = parse_credits(
            r#"{"data":{"total_credits":10.0,"total_usage":1.0,"extra":true},"ignored":1}"#,
        )
        .unwrap();
        assert_eq!(parsed.data.total_credits, 10.0);
        assert_eq!(parsed.data.total_usage, 1.0);
    }

    #[test]
    fn parse_credits_rejects_missing_data_wrapper() {
        assert!(parse_credits(r#"{"total_credits":100.5,"total_usage":25.75}"#).is_err());
    }

    #[test]
    fn parse_credits_rejects_missing_field() {
        assert!(parse_credits(r#"{"data":{"total_credits":100.5}}"#).is_err());
    }

    #[test]
    fn parse_credits_rejects_null_usage() {
        assert!(parse_credits(r#"{"data":{"total_credits":100.5,"total_usage":null}}"#).is_err());
    }

    #[test]
    fn parse_credits_rejects_string_numbers() {
        assert!(
            parse_credits(r#"{"data":{"total_credits":"100.5","total_usage":"25.75"}}"#).is_err()
        );
    }

    #[test]
    fn parse_credits_rejects_truncated_json() {
        assert!(parse_credits(r#"{"data":{"total_credits":100.5,"total_usage":"#).is_err());
    }

    #[test]
    fn parse_credits_rejects_empty_object() {
        assert!(parse_credits("{}").is_err());
    }

    #[test]
    fn remaining_credits_zero_and_overdrawn() {
        assert_eq!(remaining_credits(10.0, 10.0), 0.0);
        assert_eq!(remaining_credits(10.0, 12.5), -2.5);
    }

    #[test]
    fn format_credits_text_happy_path() {
        assert_eq!(
            format_credits_text(13.74, 636.26, 650.0),
            "$13.74 remaining  ($636.26 used of $650.00)"
        );
    }

    #[test]
    fn format_credits_text_negative_remaining_puts_dollar_before_sign() {
        // Behaviour: overdrawn balances render as `$-1.50`, not `-$1.50`.
        assert_eq!(
            format_credits_text(-1.5, 11.5, 10.0),
            "$-1.50 remaining  ($11.50 used of $10.00)"
        );
    }

    #[test]
    fn format_credits_json_pretty_prints_fields() {
        let json = format_credits_json(13.74, 636.26, 650.0).unwrap();
        assert_eq!(
            json,
            "{\n  \"remaining\": 13.74,\n  \"used\": 636.26,\n  \"total\": 650.0\n}"
        );
    }

    #[test]
    fn credits_json_keeps_raw_f64_while_text_rounds() {
        // 650 - 636.26 is not binary-exact. Text rounds to 2dp; JSON dumps the
        // raw f64, so the two outputs can disagree on remaining.
        let used = 636.26;
        let total = 650.0;
        let remaining = remaining_credits(total, used);
        let text = format_credits_text(remaining, used, total);
        let json = format_credits_json(remaining, used, total).unwrap();
        assert_ne!(remaining, 13.74);
        assert!(text.starts_with("$13.74 remaining"), "text was {text:?}");
        assert!(
            json.contains("\"remaining\": 13.740000000000009"),
            "json did not keep the raw f64 remaining {remaining}: {json}"
        );
    }

    #[test]
    fn low_balance_warns_strictly_below_five() {
        assert!(low_balance_message(4.99, false).is_some());
        assert!(low_balance_message(0.0, false).is_some());
        assert!(low_balance_message(-1.0, true).is_some());
        assert!(low_balance_message(5.0, false).is_none());
        assert!(low_balance_message(5.01, true).is_none());
    }

    #[test]
    fn low_balance_tty_uses_emoji_non_tty_is_plain() {
        assert_eq!(
            low_balance_message(1.0, true).as_deref(),
            Some("⚠️  Low — top up at https://openrouter.ai/credits")
        );
        assert_eq!(
            low_balance_message(1.0, false).as_deref(),
            Some("Low - top up at https://openrouter.ai/credits")
        );
    }

    #[test]
    fn display_rounding_can_show_five_dollars_and_still_warn() {
        // 4.995 rounds to `$5.00` at 2dp but is still < 5.0, so a warning fires
        // on a line that looks like a $5.00 remaining balance.
        let remaining = 4.995;
        assert_eq!(
            format_credits_text(remaining, 0.005, 5.0),
            "$5.00 remaining  ($0.01 used of $5.00)"
        );
        assert!(low_balance_message(remaining, false).is_some());
    }

    #[test]
    fn display_rounding_can_hide_a_balance_just_above_threshold() {
        let remaining = 5.004;
        assert_eq!(
            format_credits_text(remaining, 0.0, 5.004),
            "$5.00 remaining  ($0.00 used of $5.00)"
        );
        assert!(low_balance_message(remaining, false).is_none());
    }

    #[test]
    fn parse_usage_happy_path_from_docs() {
        let parsed = parse_usage(
            r#"{"data":{"usage_daily":0.11,"usage_weekly":1.45,"usage_monthly":10.75}}"#,
        )
        .unwrap();
        assert_eq!(parsed.data.usage_daily, 0.11);
        assert_eq!(parsed.data.usage_weekly, 1.45);
        assert_eq!(parsed.data.usage_monthly, 10.75);
    }

    #[test]
    fn parse_usage_ignores_byok_fields_and_understates_combined_spend() {
        // Official GET /key example includes both usage_* and byok_usage_*.
        // Current structs keep OpenRouter usage only, so reported spend drops
        // the BYOK component.
        let parsed = parse_usage(
            r#"{
                "data": {
                    "label": "sk-or-v1-au7...890",
                    "limit": 100,
                    "limit_remaining": 74.5,
                    "usage": 25.5,
                    "usage_daily": 25.5,
                    "usage_weekly": 25.5,
                    "usage_monthly": 25.5,
                    "byok_usage": 17.38,
                    "byok_usage_daily": 17.38,
                    "byok_usage_weekly": 17.38,
                    "byok_usage_monthly": 17.38,
                    "is_free_tier": false
                }
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.data.usage_daily, 25.5);
        assert_eq!(
            normalize_usage(parsed.data.usage_daily),
            25.5,
            "BYOK daily 17.38 is dropped; combined daily spend 42.88 is never shown"
        );
    }

    #[test]
    fn parse_usage_accepts_integers_and_scientific_notation() {
        let parsed =
            parse_usage(r#"{"data":{"usage_daily":0,"usage_weekly":1,"usage_monthly":1.5e1}}"#)
                .unwrap();
        assert_eq!(parsed.data.usage_daily, 0.0);
        assert_eq!(parsed.data.usage_weekly, 1.0);
        assert_eq!(parsed.data.usage_monthly, 15.0);
    }

    #[test]
    fn parse_usage_rejects_missing_period() {
        assert!(parse_usage(r#"{"data":{"usage_daily":1.0,"usage_weekly":2.0}}"#).is_err());
    }

    #[test]
    fn parse_usage_rejects_null_and_string_fields() {
        assert!(
            parse_usage(r#"{"data":{"usage_daily":null,"usage_weekly":0,"usage_monthly":0}}"#)
                .is_err()
        );
        assert!(
            parse_usage(
                r#"{"data":{"usage_daily":"0.11","usage_weekly":1.45,"usage_monthly":10.75}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn parse_usage_rejects_unwrapped_payload() {
        assert!(
            parse_usage(r#"{"usage_daily":1.0,"usage_weekly":2.0,"usage_monthly":3.0}"#).is_err()
        );
    }

    #[test]
    fn normalize_usage_is_identity_including_old_heuristic_boundary() {
        // v0.1 divided values >= 100 by 100. Current behaviour returns the
        // API dollars unchanged, including that former boundary.
        for value in [0.0, 0.11, 99.9, 100.0, 636.26, 10_000.0, -2.0] {
            assert_eq!(normalize_usage(value), value);
        }
    }

    #[test]
    fn format_usage_text_aligns_labels() {
        assert_eq!(
            format_usage_text(0.11, 1.45, 10.75),
            "Daily:   $0.11\nWeekly:  $1.45\nMonthly: $10.75"
        );
    }

    #[test]
    fn format_usage_json_pretty_prints_fields() {
        let json = format_usage_json(0.11, 1.45, 10.75).unwrap();
        assert_eq!(
            json,
            "{\n  \"daily\": 0.11,\n  \"weekly\": 1.45,\n  \"monthly\": 10.75\n}"
        );
    }

    #[test]
    fn format_usage_text_zero_and_negative() {
        assert_eq!(
            format_usage_text(0.0, 0.0, 0.0),
            "Daily:   $0.00\nWeekly:  $0.00\nMonthly: $0.00"
        );
        assert_eq!(
            format_usage_text(-0.5, 1.0, 2.0),
            "Daily:   $-0.50\nWeekly:  $1.00\nMonthly: $2.00"
        );
    }

    #[test]
    fn resolve_base_url_default_empty_and_override() {
        assert_eq!(resolve_base_url(None), "https://openrouter.ai");
        assert_eq!(
            resolve_base_url(Some(String::from("https://example.test"))),
            "https://example.test"
        );
        // Empty env is Ok(""), not unset, so the default is skipped.
        assert_eq!(resolve_base_url(Some(String::new())), "");
    }

    #[test]
    fn endpoints_join_with_single_slash_and_keep_legacy_usage_path() {
        assert_eq!(
            credits_endpoint("https://openrouter.ai"),
            "https://openrouter.ai/api/v1/credits"
        );
        assert_eq!(
            usage_endpoint("https://openrouter.ai"),
            "https://openrouter.ai/api/v1/auth/key"
        );
    }

    #[test]
    fn endpoints_double_slash_when_base_has_trailing_slash() {
        assert_eq!(
            credits_endpoint("https://openrouter.ai/"),
            "https://openrouter.ai//api/v1/credits"
        );
        assert_eq!(
            usage_endpoint("https://openrouter.ai/"),
            "https://openrouter.ai//api/v1/auth/key"
        );
    }

    #[test]
    fn empty_base_url_produces_root_relative_paths() {
        assert_eq!(credits_endpoint(""), "/api/v1/credits");
        assert_eq!(usage_endpoint(""), "/api/v1/auth/key");
    }

    #[test]
    fn api_key_from_env_treats_empty_as_missing_but_keeps_whitespace() {
        assert_eq!(
            api_key_from_env(Some(String::from("sk-or-v1-x"))),
            Some(String::from("sk-or-v1-x"))
        );
        assert_eq!(api_key_from_env(Some(String::new())), None);
        assert_eq!(api_key_from_env(None), None);
        // Env is not trimmed; keychain stdout is. A padded env key is used as-is.
        assert_eq!(
            api_key_from_env(Some(String::from("  sk-or-v1-x  "))),
            Some(String::from("  sk-or-v1-x  "))
        );
    }

    #[test]
    fn api_key_from_keychain_stdout_trims_and_rejects_blank() {
        assert_eq!(
            api_key_from_keychain_stdout(b"sk-or-v1-x\n").unwrap(),
            "sk-or-v1-x"
        );
        assert_eq!(
            api_key_from_keychain_stdout(b"  sk-or-v1-x  \n").unwrap(),
            "sk-or-v1-x"
        );
        assert!(matches!(
            api_key_from_keychain_stdout(b""),
            Err(AppError::ApiKeyMissing)
        ));
        assert!(matches!(
            api_key_from_keychain_stdout(b" \n\t"),
            Err(AppError::ApiKeyMissing)
        ));
    }

    #[test]
    fn api_key_missing_display_mentions_save_command() {
        assert_eq!(
            AppError::ApiKeyMissing.to_string(),
            "Error: API key not found. Run: stips key save <your-key>"
        );
        assert_eq!(
            AppError::Message(String::from("Error: failed to open URL")).to_string(),
            "Error: failed to open URL"
        );
    }
}
