use std::fmt;
use std::io::{self, IsTerminal};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};
use serde::Deserialize;

const OPENROUTER_CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const OPENROUTER_KEY_URL: &str = "https://openrouter.ai/api/v1/auth/key";
const OPENROUTER_KEYS_PAGE: &str = "https://openrouter.ai/keys";
const OPENROUTER_TOPUP_PAGE: &str = "https://openrouter.ai/credits";
const KEYCHAIN_SERVICE: &str = "openrouter-api-key";
const KEYCHAIN_ACCOUNT: &str = "openrouter";

#[derive(Parser, Debug)]
#[command(name = "stips", about = "OpenRouter credits and usage CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Credits,
    Usage,
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
    Silent,
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
            Self::Silent => Ok(()),
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if !matches!(err, AppError::Silent) {
                eprintln!("{err}");
            }
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    let is_tty = io::stdout().is_terminal();

    match cli.command {
        None | Some(Commands::Credits) => cmd_credits(is_tty),
        Some(Commands::Usage) => cmd_usage(),
        Some(Commands::Key { command }) => match command {
            KeyCommands::Open => cmd_key_open(),
            KeyCommands::Save { key } => cmd_key_save(&key),
        },
    }
}

fn cmd_credits(is_tty: bool) -> Result<(), AppError> {
    let key = read_api_key_from_keychain()?;
    let response = request_json::<CreditsEnvelope>(OPENROUTER_CREDITS_URL, &key)?;
    let remaining = response.data.total_credits - response.data.total_usage;

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
        return Err(AppError::Silent);
    }

    Ok(())
}

fn cmd_usage() -> Result<(), AppError> {
    let key = read_api_key_from_keychain()?;
    let response = request_json::<UsageEnvelope>(OPENROUTER_KEY_URL, &key)?;

    println!(
        "Daily:   ${:.2}",
        normalize_usage(response.data.usage_daily)
    );
    println!(
        "Weekly:  ${:.2}",
        normalize_usage(response.data.usage_weekly)
    );
    println!(
        "Monthly: ${:.2}",
        normalize_usage(response.data.usage_monthly)
    );

    Ok(())
}

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

fn request_json<T: for<'de> Deserialize<'de>>(url: &str, api_key: &str) -> Result<T, AppError> {
    let response = ureq::get(url)
        .header("Authorization", &format!("Bearer {api_key}"))
        .call()
        .map_err(|err| AppError::Message(format!("Error: {err}")))?;

    let mut body = response.into_body();
    body.read_json::<T>()
        .map_err(|err| AppError::Message(format!("Error: {err}")))
}

fn normalize_usage(value: f64) -> f64 {
    if value >= 100.0 { value / 100.0 } else { value }
}
