use std::fmt;
use std::io::{self, IsTerminal};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use serde::Deserialize;

const OPENROUTER_KEYS_PAGE: &str = "https://openrouter.ai/keys";
const OPENROUTER_TOPUP_PAGE: &str = "https://openrouter.ai/credits";
const KEYCHAIN_SERVICE: &str = "openrouter-api-key";
const KEYCHAIN_ACCOUNT: &str = "openrouter";

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

#[derive(Debug, Deserialize)]
struct CreditsEnvelope {
    data: CreditsData,
}

#[derive(Debug, Deserialize)]
struct CreditsData {
    total_credits: f64,
    total_usage: f64,
}

#[derive(Debug, Deserialize)]
struct UsageEnvelope {
    data: UsageData,
}

#[derive(Debug, Deserialize)]
struct UsageData {
    usage_daily: f64,
    usage_weekly: f64,
    usage_monthly: f64,
}

#[derive(serde::Serialize)]
struct CreditsOutput {
    remaining: f64,
    used: f64,
    total: f64,
}

#[derive(serde::Serialize)]
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
    let url = format!("{}/api/v1/credits", base_url());
    let response = request_json::<CreditsEnvelope>(&url, &key)?;
    let remaining = response.data.total_credits - response.data.total_usage;

    if json {
        let out = CreditsOutput {
            remaining,
            used: response.data.total_usage,
            total: response.data.total_credits,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| AppError::Message(format!("Error: {e}")))?
        );
    } else {
        println!(
            "${remaining:.2} remaining  (${:.2} used of ${:.2})",
            response.data.total_usage, response.data.total_credits
        );
        if remaining < 5.0 {
            if is_tty {
                eprintln!("⚠️  Low — top up at {OPENROUTER_TOPUP_PAGE}");
            } else {
                eprintln!("Low - top up at {OPENROUTER_TOPUP_PAGE}");
            }
        }
    }

    Ok(())
}

fn cmd_usage(json: bool) -> Result<(), AppError> {
    let key = read_api_key_from_keychain()?;
    let url = format!("{}/api/v1/auth/key", base_url());
    let response = request_json::<UsageEnvelope>(&url, &key)?;

    let daily = normalize_usage(response.data.usage_daily);
    let weekly = normalize_usage(response.data.usage_weekly);
    let monthly = normalize_usage(response.data.usage_monthly);

    if json {
        let out = UsageOutput { daily, weekly, monthly };
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| AppError::Message(format!("Error: {e}")))?
        );
    } else {
        println!("Daily:   ${daily:.2}");
        println!("Weekly:  ${weekly:.2}");
        println!("Monthly: ${monthly:.2}");
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
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
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

        let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if key.is_empty() {
            Err(AppError::ApiKeyMissing)
        } else {
            Ok(key)
        }
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
    std::env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| String::from("https://openrouter.ai"))
}

/// Returns usage in USD as returned by `GET /api/v1/auth/key`.
/// The API always returns dollar-denominated floats; no unit conversion is needed.
/// Ref: https://openrouter.ai/docs/api/api-reference/overview
fn normalize_usage(value: f64) -> f64 {
    value
}
