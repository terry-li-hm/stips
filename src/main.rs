use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, IsTerminal, Read};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::sync::mpsc;
use ureq::http::{Request, Response, header};
use ureq::unversioned::resolver::{DefaultResolver, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

const OPENROUTER_KEYS_PAGE: &str = "https://openrouter.ai/keys";
const OPENROUTER_TOPUP_PAGE: &str = "https://openrouter.ai/credits";
const KEYCHAIN_SERVICE: &str = "openrouter-api-key";
const KEYCHAIN_ACCOUNT: &str = "openrouter";
const LOW_BALANCE_THRESHOLD: f64 = 5.0;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const RESPONSE_CAP: usize = 256 * 1024;

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
}

#[derive(Clone, Copy, Debug)]
enum Endpoint {
    Credits,
    Usage,
}

impl Endpoint {
    fn url(self) -> &'static str {
        match self {
            Self::Credits => "https://openrouter.ai/api/v1/credits",
            Self::Usage => "https://openrouter.ai/api/v1/auth/key",
        }
    }
}

#[derive(Debug)]
enum AppError {
    ApiKeyMissing,
    CredentialWriteRetired,
    HttpStatus(u16),
    Message(&'static str),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let problem = match self {
            Self::ApiKeyMissing => "API key not found",
            Self::CredentialWriteRetired => "This credential command has been retired",
            Self::HttpStatus(code) => return write!(f, "Error: OpenRouter returned HTTP {code}."),
            Self::Message(msg) => return write!(f, "Error: {msg}"),
        };
        write!(
            f,
            "Error: {problem}. Set OPENROUTER_API_KEY or use macOS Keychain Access \
             (service: {KEYCHAIN_SERVICE}, account: {KEYCHAIN_ACCOUNT})."
        )
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
    #[serde(default)]
    byok_usage_daily: f64,
    #[serde(default)]
    byok_usage_weekly: f64,
    #[serde(default)]
    byok_usage_monthly: f64,
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

fn parse_cli(args: impl IntoIterator<Item = OsString>) -> Result<Cli, AppError> {
    let mut args = args.into_iter();
    let program = args.next();
    let first = args.next();
    let second = args.next();
    // Reject the retired syntax before clap can include any trailing arguments
    // in a parse error. Do not consume or interpret the trailing arguments.
    if first.as_deref() == Some(OsStr::new("key")) && second.as_deref() == Some(OsStr::new("save"))
    {
        return Err(AppError::CredentialWriteRetired);
    }
    Ok(Cli::parse_from(
        program.into_iter().chain(first).chain(second).chain(args),
    ))
}

fn run() -> Result<(), AppError> {
    let (endpoint, json) = match parse_cli(std::env::args_os())?.command {
        None => (Endpoint::Credits, false),
        Some(Commands::Credits { json }) => (Endpoint::Credits, json),
        Some(Commands::Usage { json }) => (Endpoint::Usage, json),
        Some(Commands::Key {
            command: KeyCommands::Open,
        }) => return cmd_key_open(),
    };

    let (output, warning) = run_query(
        endpoint,
        json,
        io::stdout().is_terminal(),
        std::env::var_os("OPENROUTER_BASE_URL").is_some(),
        read_api_key,
        ureq::Agent::run,
    )?;
    println!("{output}");
    if let Some(warning) = warning {
        eprintln!("{warning}");
    }
    Ok(())
}

type HttpResult = Result<Response<ureq::Body>, ureq::Error>;

fn run_query(
    endpoint: Endpoint,
    json: bool,
    is_tty: bool,
    has_endpoint_override: bool,
    read_key: impl FnOnce() -> Result<String, AppError>,
    send: impl FnOnce(&ureq::Agent, Request<()>) -> HttpResult + Send + 'static,
) -> Result<(String, Option<String>), AppError> {
    if has_endpoint_override {
        return Err(AppError::Message(
            "OPENROUTER_BASE_URL is no longer supported. Unset it to use OpenRouter.",
        ));
    }
    let key = read_key()?;
    let bytes = request_body(endpoint, &key, send)?;
    match endpoint {
        Endpoint::Credits => credits_report(decode::<CreditsEnvelope>(&bytes)?.data, json, is_tty),
        Endpoint::Usage => usage_report(decode::<UsageEnvelope>(&bytes)?.data, json),
    }
}

fn finite(value: f64) -> Result<f64, AppError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AppError::Message("Unrepresentable OpenRouter report."))
    }
}

impl TryFrom<CreditsData> for CreditsOutput {
    type Error = AppError;

    fn try_from(data: CreditsData) -> Result<Self, Self::Error> {
        let used = finite(data.total_usage)?;
        let total = finite(data.total_credits)?;
        Ok(Self {
            remaining: finite(remaining_credits(total, used))?,
            used,
            total,
        })
    }
}

impl TryFrom<UsageData> for UsageOutput {
    type Error = AppError;

    fn try_from(data: UsageData) -> Result<Self, Self::Error> {
        let period = |usage, byok| finite(combined_usage(finite(usage)?, finite(byok)?));
        Ok(Self {
            daily: period(data.usage_daily, data.byok_usage_daily)?,
            weekly: period(data.usage_weekly, data.byok_usage_weekly)?,
            monthly: period(data.usage_monthly, data.byok_usage_monthly)?,
        })
    }
}

fn credits_report(
    data: CreditsData,
    json: bool,
    is_tty: bool,
) -> Result<(String, Option<String>), AppError> {
    let report = CreditsOutput::try_from(data)?;
    if json {
        Ok((format_json(&report)?, None))
    } else {
        Ok((
            format_credits_text(report.remaining, report.used, report.total),
            low_balance_message(report.remaining, is_tty),
        ))
    }
}

fn usage_report(data: UsageData, json: bool) -> Result<(String, Option<String>), AppError> {
    let report = UsageOutput::try_from(data)?;
    let output = if json {
        format_json(&report)?
    } else {
        format_usage_text(report.daily, report.weekly, report.monthly)
    };
    Ok((output, None))
}

#[cfg(not(target_os = "macos"))]
fn cmd_key_open() -> Result<(), AppError> {
    println!("{OPENROUTER_KEYS_PAGE}");
    Err(AppError::Message(
        "stips key open requires macOS. Visit the URL above in your browser.",
    ))
}

#[cfg(target_os = "macos")]
fn cmd_key_open() -> Result<(), AppError> {
    println!("Opening {OPENROUTER_KEYS_PAGE}");
    let status = Command::new("open")
        .arg(OPENROUTER_KEYS_PAGE)
        .status()
        .map_err(|_| AppError::Message("failed to run open"))?;

    if status.success() {
        Ok(())
    } else {
        Err(AppError::Message("failed to open URL"))
    }
}

fn read_api_key() -> Result<String, AppError> {
    if let Some(key) = api_key_from_env(std::env::var("OPENROUTER_API_KEY").ok()) {
        return Ok(key);
    }

    #[cfg(not(target_os = "macos"))]
    return Err(AppError::ApiKeyMissing);

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

// ureq 3.2 strips Content-Length before exposing a gzip response. Check it
// on the plaintext side of the normal TLS/proxy chain without changing any
// bytes. ureq still owns HTTP framing, header parsing and decompression.
#[derive(Debug)]
struct WireLengthGuard(Endpoint);

impl<T: Transport> Connector<T> for WireLengthGuard {
    type Out = CheckedTransport<T>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let Some(mut inner) = chained else {
            return Ok(None);
        };
        // The default CONNECT connector can recurse for the proxy itself.
        // Inspect only the pinned origin response, never the proxy tunnel.
        let checking = details.uri == self.0.url() && !checked_head(inner.buffers().input())?;
        Ok(Some(CheckedTransport { inner, checking }))
    }
}

#[derive(Debug)]
struct CheckedTransport<T> {
    inner: T,
    checking: bool,
}

impl<T: Transport> Transport for CheckedTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }
    fn transmit_output(&mut self, n: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(n, timeout)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let progress = self.inner.await_input(timeout)?;
        if self.checking {
            self.checking = !checked_head(self.inner.buffers().input())?;
        }
        Ok(progress)
    }
    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

// Incomplete heads stay in ureq's input buffer. Inspect complete lines only,
// including a final response coalesced with a 100 Continue. Stop at the blank
// line: neither header values nor any following body need to be copied.
fn checked_head(input: &[u8]) -> Result<bool, ureq::Error> {
    let mut lines = input
        .split_inclusive(|&b| b == b'\n')
        .map_while(|line| line.strip_suffix(b"\n"))
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    while let Some(status) = lines.find(|line| !line.is_empty()) {
        let code = status
            .split(|b| b.is_ascii_whitespace())
            .filter(|s| !s.is_empty())
            .nth(1)
            .unwrap_or_default();
        let interim = code == b"100";
        let success = code.starts_with(b"2");
        for line in lines.by_ref() {
            if line.is_empty() {
                if interim {
                    break;
                }
                return Ok(true);
            }
            if success
                && let Some(i) = line.iter().position(|&b| b == b':')
                && line[..i].eq_ignore_ascii_case(b"content-length")
            {
                check_length(&line[i + 1..])?;
            }
        }
    }
    Ok(false)
}

fn check_length(value: &[u8]) -> Result<(), ureq::Error> {
    let n = std::str::from_utf8(value.trim_ascii())
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    if n > RESPONSE_CAP as u64 {
        return Err(ureq::Error::BodyExceedsLimit(RESPONSE_CAP as u64));
    }
    Ok(())
}

fn request_body(
    endpoint: Endpoint,
    api_key: &str,
    send: impl FnOnce(&ureq::Agent, Request<()>) -> HttpResult + Send + 'static,
) -> Result<Vec<u8>, AppError> {
    let mut bearer = header::HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| AppError::Message("Invalid API key format."))?;
    bearer.set_sensitive(true);
    let request = Request::get(endpoint.url())
        .header(header::AUTHORIZATION, bearer)
        .body(())
        .map_err(|_| AppError::Message("Could not build OpenRouter request."))?;
    let agent = billing_agent(
        endpoint,
        DefaultConnector::default(),
        DefaultResolver::default(),
    );
    fetch_before(Instant::now() + REQUEST_TIMEOUT, move || {
        send(&agent, request)
    })
}

fn billing_agent(
    endpoint: Endpoint,
    connector: impl Connector,
    resolver: impl Resolver,
) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(REQUEST_TIMEOUT))
        .build();
    ureq::Agent::with_parts(config, connector.chain(WireLengthGuard(endpoint)), resolver)
}

// ureq can remain inside a blocking send/read after its global timeout. The
// one-shot CLI owns only the receiver; on expiry it returns to main without
// joining the I/O worker. Process exit releases the socket and the worker.
fn fetch_before(
    deadline: Instant,
    send: impl FnOnce() -> HttpResult + Send + 'static,
) -> Result<Vec<u8>, AppError> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let result = send()
                .map_err(|err| sanitized_http_error(err, "OpenRouter request failed."))
                .and_then(|response| read_success(response, deadline));
            let _ = tx.send(result);
        })
        .map_err(|_| AppError::Message("OpenRouter request failed."))?;
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(AppError::Message("OpenRouter request timed out."))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(AppError::Message("OpenRouter request failed."))
        }
        Ok(_) if Instant::now() >= deadline => {
            Err(AppError::Message("OpenRouter request timed out."))
        }
        Ok(result) => result,
    }
}

fn read_success(
    mut response: Response<ureq::Body>,
    deadline: Instant,
) -> Result<Vec<u8>, AppError> {
    // With redirects disabled, ureq returns the 3xx response to us. Do not
    // inspect its Location or try to parse its body as a successful response.
    let status = response.status();
    if status.is_redirection() {
        return Err(AppError::Message("OpenRouter HTTP redirect refused."));
    }
    if !status.is_success() {
        return Err(AppError::HttpStatus(status.as_u16()));
    }
    for length in response.headers().get_all(header::CONTENT_LENGTH) {
        check_length(length.as_bytes())
            .map_err(|e| sanitized_http_error(e, "Invalid OpenRouter response length."))?;
    }
    read_bounded(response.body_mut().as_reader(), deadline, Instant::now)
}

fn read_bounded(
    reader: impl Read,
    deadline: Instant,
    now: impl Fn() -> Instant,
) -> Result<Vec<u8>, AppError> {
    let mut reader = reader.take(RESPONSE_CAP as u64 + 1);
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        if now() >= deadline {
            return Err(AppError::Message("OpenRouter request timed out."));
        }
        let n = reader.read(&mut chunk).map_err(|err| {
            sanitized_http_error(err.into(), "Could not read OpenRouter response.")
        })?;
        if bytes.len() + n > RESPONSE_CAP {
            return Err(AppError::Message("OpenRouter response too large."));
        }
        if n == 0 {
            // EOF itself may have blocked. Never accept a late final read.
            if now() >= deadline {
                return Err(AppError::Message("OpenRouter request timed out."));
            }
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(bytes).map_err(|_| AppError::Message("Invalid OpenRouter response."))
}

fn sanitized_http_error(error: ureq::Error, fallback: &'static str) -> AppError {
    match error {
        ureq::Error::BodyExceedsLimit(_) => AppError::Message("OpenRouter response too large."),
        ureq::Error::Timeout(_) => AppError::Message("OpenRouter request timed out."),
        ureq::Error::Io(err) if err.kind() == io::ErrorKind::TimedOut => {
            AppError::Message("OpenRouter request timed out.")
        }
        _ => AppError::Message(fallback),
    }
}

/// Returns usage in USD as returned by `GET /api/v1/auth/key`.
/// The API always returns dollar-denominated floats; no unit conversion is needed.
/// Ref: https://openrouter.ai/docs/api/api-reference/overview
fn normalize_usage(value: f64) -> f64 {
    value
}

fn combined_usage(openrouter: f64, byok: f64) -> f64 {
    normalize_usage(openrouter) + normalize_usage(byok)
}

fn remaining_credits(total_credits: f64, total_usage: f64) -> f64 {
    total_credits - total_usage
}

fn format_usd(amount: f64) -> String {
    if amount < 0.0 {
        format!("-${:.2}", amount.abs())
    } else {
        format!("${amount:.2}")
    }
}

fn format_credits_text(remaining: f64, used: f64, total: f64) -> String {
    format!(
        "{} remaining  (${used:.2} used of ${total:.2})",
        format_usd(remaining)
    )
}

fn format_json(report: &impl serde::Serialize) -> Result<String, AppError> {
    serde_json::to_string_pretty(report)
        .map_err(|_| AppError::Message("Could not format JSON output."))
}

fn format_usage_text(daily: f64, weekly: f64, monthly: f64) -> String {
    format!("Daily:   ${daily:.2}\nWeekly:  ${weekly:.2}\nMonthly: ${monthly:.2}")
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

fn api_key_from_env(from_env: Option<String>) -> Option<String> {
    from_env.filter(|key| !key.is_empty())
}

#[cfg(any(target_os = "macos", test))]
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
    fn format_credits_text_negative_remaining_puts_sign_before_dollar() {
        assert_eq!(
            format_credits_text(-1.5, 11.5, 10.0),
            "-$1.50 remaining  ($11.50 used of $10.00)"
        );
    }

    #[test]
    fn format_credits_json_pretty_prints_fields() {
        let json = format_json(&CreditsOutput {
            remaining: 13.74,
            used: 636.26,
            total: 650.0,
        })
        .unwrap();
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
        let json = format_json(&CreditsOutput {
            remaining,
            used,
            total,
        })
        .unwrap();
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
    fn parse_usage_includes_byok_fields_in_combined_spend() {
        // Official GET /key example includes both usage_* and byok_usage_*.
        // Combined spend includes the BYOK component.
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
        assert_eq!(parsed.data.byok_usage_daily, 17.38);
        let daily = combined_usage(parsed.data.usage_daily, parsed.data.byok_usage_daily);
        let weekly = combined_usage(parsed.data.usage_weekly, parsed.data.byok_usage_weekly);
        let monthly = combined_usage(parsed.data.usage_monthly, parsed.data.byok_usage_monthly);
        assert_eq!(format!("{daily:.2}"), "42.88");
        assert_eq!(format!("{weekly:.2}"), "42.88");
        assert_eq!(format!("{monthly:.2}"), "42.88");
        assert_ne!(daily, parsed.data.usage_daily);
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
        let json = format_json(&UsageOutput {
            daily: 0.11,
            weekly: 1.45,
            monthly: 10.75,
        })
        .unwrap();
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
    fn retired_key_command_stops_before_consuming_trailing_args() {
        let args = ["stips", "key", "save"]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::from_fn(|| {
                panic!("read a retired credential argument")
            }));
        assert!(matches!(
            parse_cli(args),
            Err(AppError::CredentialWriteRetired)
        ));
    }

    const SYNTHETIC_KEY: &str = "offline-test-credential-sentinel";
    const MOCK_CREDITS: &str = r#"{"data":{"total_credits":10,"total_usage":8}}"#;

    // The command only prints a successful (output, warning) pair. Exercise
    // that same boundary with an in-memory transport and a recording reader.
    #[derive(Default, Clone)]
    struct ReadCounts {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    struct Probe {
        data: io::Cursor<Vec<u8>>,
        counts: ReadCounts,
        chunk: usize,
        fail_at_eof: bool,
    }

    impl Probe {
        fn new(data: Vec<u8>) -> (Self, ReadCounts) {
            let counts = ReadCounts::default();
            (
                Self {
                    data: io::Cursor::new(data),
                    counts: counts.clone(),
                    chunk: usize::MAX,
                    fail_at_eof: false,
                },
                counts,
            )
        }
    }

    use std::sync::atomic::Ordering::Relaxed;

    impl Read for Probe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.counts.calls.fetch_add(1, Relaxed);
            let limit = buf.len().min(self.chunk);
            let n = self.data.read(&mut buf[..limit])?;
            self.counts.bytes.fetch_add(n, Relaxed);
            if n == 0 && self.fail_at_eof {
                Err(io::Error::other(SYNTHETIC_KEY))
            } else {
                Ok(n)
            }
        }
    }

    fn probe_query(
        probe: Probe,
        length: Option<usize>,
        status: u16,
        json: bool,
    ) -> Result<(String, Option<String>), AppError> {
        run_query(
            Endpoint::Credits,
            json,
            false,
            false,
            stub_key,
            move |_, _| {
                let mut response = Response::builder().status(status);
                if let Some(n) = length {
                    response = response.header(header::CONTENT_LENGTH, n);
                }
                Ok(response.body(ureq::Body::builder().reader(probe)).unwrap())
            },
        )
    }

    #[test]
    fn zg_report_exact_cap_valid_response() {
        let mut data = MOCK_CREDITS.as_bytes().to_vec();
        data.resize(RESPONSE_CAP, b' ');
        let (probe, counts) = Probe::new(data);
        let (output, warning) = probe_query(probe, Some(RESPONSE_CAP), 200, true).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap()["remaining"],
            2.0
        );
        assert_eq!(warning, None);
        assert_eq!(counts.bytes.load(Relaxed), RESPONSE_CAP);
    }

    #[test]
    fn zg_report_declared_oversize_and_error_status_skip_body() {
        for (status, diagnostic) in [
            (200, "Error: OpenRouter response too large."),
            (403, "Error: OpenRouter returned HTTP 403."),
        ] {
            let (probe, counts) = Probe::new(MOCK_CREDITS.as_bytes().to_vec());
            let err = probe_query(probe, Some(RESPONSE_CAP + 1), status, false).unwrap_err();
            assert_eq!(err.to_string(), diagnostic);
            assert_eq!(counts.calls.load(Relaxed), 0);
        }
    }

    #[test]
    fn zg_report_actual_cap_independent_of_header() {
        for (length, size) in [(None, RESPONSE_CAP + 1), (Some(1), RESPONSE_CAP * 2)] {
            let (probe, counts) = Probe::new(vec![b' '; size]);
            let err = probe_query(probe, length, 200, true).unwrap_err();
            assert_eq!(err.to_string(), "Error: OpenRouter response too large.");
            assert_eq!(counts.bytes.load(Relaxed), RESPONSE_CAP + 1);
        }
    }

    #[test]
    fn zg_report_absolute_deadline_does_not_join_blocked_io() {
        struct BlockedRead {
            entered: mpsc::Sender<Instant>,
            release: std::sync::Mutex<mpsc::Receiver<()>>,
        }
        impl Read for BlockedRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                self.entered.send(Instant::now()).unwrap();
                // Bound the fixture itself so a regression cannot hang the suite.
                self.release
                    .get_mut()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Ok(0)
            }
        }
        for block_in_send in [true, false] {
            let (release, gate) = mpsc::channel();
            let (entered, started) = mpsc::channel();
            let deadline = Instant::now() + Duration::from_millis(250);
            let err = fetch_before(deadline, move || {
                let mut block = BlockedRead {
                    entered,
                    release: std::sync::Mutex::new(gate),
                };
                if block_in_send {
                    block.read(&mut [0])?;
                }
                Ok(Response::new(ureq::Body::builder().reader(block)))
            })
            .unwrap_err();
            assert_eq!(err.to_string(), "Error: OpenRouter request timed out.");
            assert!(started.try_recv().unwrap() < deadline);
            assert!(Instant::now() >= deadline);
            // Only now can the blocked send or read return.
            release.send(()).unwrap();
        }
    }

    // Supply only DNS and the byte stream. Agent::run still executes ureq's
    // actual HTTP framing, header removal and gzip decoder, without a socket.
    #[derive(Debug)]
    struct FixtureResolver;
    impl Resolver for FixtureResolver {
        fn resolve(
            &self,
            _: &ureq::http::Uri,
            _: &ureq::config::Config,
            _: NextTimeout,
        ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
            let mut addrs = self.empty();
            addrs.push(([127, 0, 0, 1], 443).into());
            Ok(addrs)
        }
    }

    #[derive(Debug)]
    struct WireFixture {
        declared: usize,
        gzip: Vec<u8>,
        body_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    #[derive(Debug)]
    struct FixtureStream {
        chunks: std::vec::IntoIter<Vec<u8>>,
        buffers: ureq::unversioned::transport::LazyBuffers,
        body_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Connector for WireFixture {
        type Out = FixtureStream;
        fn connect(
            &self,
            _: &ConnectionDetails,
            _: Option<()>,
        ) -> Result<Option<FixtureStream>, ureq::Error> {
            let head = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\ncOnTeNt-Le";
            let tail = format!("NgTh: {}\r\n\r\n", self.declared);
            Ok(Some(FixtureStream {
                chunks: vec![head.to_vec(), tail.into_bytes(), self.gzip.clone()].into_iter(),
                buffers: ureq::unversioned::transport::LazyBuffers::new(1024, 1024),
                body_reads: self.body_reads.clone(),
            }))
        }
    }
    impl Transport for FixtureStream {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }
        fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
            Ok(())
        }
        fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
            if self.chunks.len() == 1 {
                self.body_reads.fetch_add(1, Relaxed);
            }
            let Some(chunk) = self.chunks.next() else {
                return Ok(false);
            };
            self.buffers.input_append_buf()[..chunk.len()].copy_from_slice(&chunk);
            self.buffers.input_appended(chunk.len());
            Ok(true)
        }
        fn is_tls(&self) -> bool {
            true
        }
        fn is_open(&mut self) -> bool {
            true
        }
    }

    #[test]
    fn zg_report_gzip_wire_and_decoded_limits_are_independent() {
        // gzip(MOCK_CREDITS), and a 290-byte gzip member containing 262,145 spaces.
        let small = vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 86, 74, 73, 44, 73, 84, 178, 170, 86, 42, 201,
            47, 73, 204, 137, 79, 46, 74, 77, 201, 44, 41, 86, 178, 50, 52, 208, 129, 10, 149, 22,
            39, 166, 167, 42, 89, 89, 212, 214, 2, 0, 129, 139, 225, 74, 45, 0, 0, 0,
        ];
        let mut expands = vec![
            31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 237, 193, 49, 1, 0, 0, 0, 194, 160, 42, 235, 159, 210,
            223, 12, 64, 1,
        ];
        expands.resize(280, 0);
        expands.extend([156, 1, 80, 20, 74, 200, 1, 0, 4, 0]);
        for (declared, gzip, decoded_oversize) in [
            (RESPONSE_CAP + 1, small.clone(), false),
            (small.len(), small, false),
            (expands.len(), expands, true),
        ] {
            let body_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let fixture = WireFixture {
                declared,
                gzip,
                body_reads: body_reads.clone(),
            };
            let agent = billing_agent(Endpoint::Credits, fixture, FixtureResolver);
            let received = agent.get(Endpoint::Credits.url()).call();
            if declared > RESPONSE_CAP {
                assert!(matches!(received, Err(ureq::Error::BodyExceedsLimit(_))));
                assert_eq!(body_reads.load(Relaxed), 0);
                continue;
            }
            let response = received.unwrap();
            assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
            assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
            let bytes = read_success(response, Instant::now() + REQUEST_TIMEOUT);
            if decoded_oversize {
                assert_eq!(
                    bytes.unwrap_err().to_string(),
                    "Error: OpenRouter response too large."
                );
            } else {
                assert_eq!(bytes.unwrap(), MOCK_CREDITS.as_bytes());
            }
            assert_eq!(body_reads.load(Relaxed), 1);
        }
    }

    #[test]
    fn zg_report_trickle_deadline_includes_final_eof() {
        let start = Instant::now();
        // Four seconds for each read: expire on the third byte, or on EOF
        // following two prompt bytes. No chunk gets a fresh 12-second budget.
        for size in [100, 2] {
            let (mut probe, counts) = Probe::new(vec![b' '; size]);
            probe.chunk = 1;
            let err = read_bounded(probe, start + REQUEST_TIMEOUT, || {
                start + Duration::from_secs(4 * counts.calls.load(Relaxed) as u64)
            })
            .unwrap_err();
            assert_eq!(err.to_string(), "Error: OpenRouter request timed out.");
            assert_eq!(counts.calls.load(Relaxed), 3);
        }
    }

    #[test]
    fn zg_report_error_after_complete_json_is_not_eof() {
        for json in [false, true] {
            let (mut probe, _) = Probe::new(MOCK_CREDITS.as_bytes().to_vec());
            probe.fail_at_eof = true;
            let err = probe_query(probe, None, 200, json).unwrap_err();
            assert_eq!(
                err.to_string(),
                "Error: Could not read OpenRouter response."
            );
        }
    }

    #[test]
    fn zg_report_invalid_or_overflow_in_either_mode_has_no_output_pair() {
        for json in [false, true] {
            for (endpoint, body, diagnostic) in [
                (
                    Endpoint::Usage,
                    r#"{"data":{"usage_daily":0,"usage_weekly":0,"usage_monthly":1e308,"byok_usage_monthly":1e308}}"#,
                    "Error: Unrepresentable OpenRouter report.",
                ),
                (
                    Endpoint::Credits,
                    r#"{"data":{"total_credits":-1e308,"total_usage":1e308}}"#,
                    "Error: Unrepresentable OpenRouter report.",
                ),
                (
                    Endpoint::Credits,
                    r#"{"data":{"total_credits":0,"total_usage":"https://private.invalid/"}}"#,
                    "Error: Invalid OpenRouter response.",
                ),
            ] {
                let err = run_query(
                    endpoint,
                    json,
                    false,
                    false,
                    stub_key,
                    mock_response(endpoint.url(), body),
                )
                .unwrap_err();
                assert_eq!(err.to_string(), diagnostic);
            }
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let err = credits_report(
                    CreditsData {
                        total_usage: value,
                        total_credits: 0.0,
                    },
                    json,
                    false,
                )
                .unwrap_err();
                assert_eq!(err.to_string(), "Error: Unrepresentable OpenRouter report.");
                let mut data =
                    parse_usage(r#"{"data":{"usage_daily":0,"usage_weekly":0,"usage_monthly":0}}"#)
                        .unwrap()
                        .data;
                data.byok_usage_weekly = value;
                assert_eq!(
                    usage_report(data, json).unwrap_err().to_string(),
                    "Error: Unrepresentable OpenRouter report."
                );
            }
        }
    }

    #[test]
    fn zg_report_finite_byok_and_overdrawn_outputs() {
        for (endpoint, body, expected_json, text, warning) in [
            (
                Endpoint::Usage,
                r#"{"data":{"usage_daily":0,"usage_weekly":1,"usage_monthly":2,"byok_usage_daily":0.125}}"#,
                serde_json::json!({"daily":0.125,"weekly":1.0,"monthly":2.0}),
                "Daily:   $0.12\nWeekly:  $1.00\nMonthly: $2.00",
                None,
            ),
            (
                Endpoint::Credits,
                r#"{"data":{"total_credits":5,"total_usage":6.5}}"#,
                serde_json::json!({"remaining":-1.5,"used":6.5,"total":5.0}),
                "-$1.50 remaining  ($6.50 used of $5.00)",
                Some("Low - top up at https://openrouter.ai/credits"),
            ),
        ] {
            let query = |json| {
                run_query(
                    endpoint,
                    json,
                    false,
                    false,
                    stub_key,
                    mock_response(endpoint.url(), body),
                )
                .unwrap()
            };
            let (output, actual_warning) = query(false);
            assert_eq!(output, text);
            assert_eq!(actual_warning.as_deref(), warning);
            let (output, actual_warning) = query(true);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&output).unwrap(),
                expected_json
            );
            assert_eq!(actual_warning, None);
        }
    }

    fn stub_key() -> Result<String, AppError> {
        Ok(SYNTHETIC_KEY.to_owned())
    }

    fn mock_response(
        url: &'static str,
        body: &'static str,
    ) -> impl FnOnce(&ureq::Agent, Request<()>) -> HttpResult {
        move |agent, request| {
            assert_eq!(request.method(), "GET");
            assert_eq!(request.uri(), url);
            let auth = &request.headers()[header::AUTHORIZATION];
            assert_eq!(auth, &format!("Bearer {SYNTHETIC_KEY}"));
            assert!(auth.is_sensitive());
            assert!(agent.config().https_only());
            Ok(Response::new(ureq::Body::builder().data(body)))
        }
    }

    #[test]
    fn https_proxy_from_env_is_preserved() {
        const CHILD: &str = "STIPS_TEST_HTTPS_PROXY_CHILD";
        const PROXY: &str = "http://offline-proxy.invalid:3128/";

        if std::env::var_os(CHILD).is_none() {
            // Set the test environment only in the child, not in the parallel test process.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::https_proxy_from_env_is_preserved"])
                .env(CHILD, "1")
                .env_remove("ALL_PROXY")
                .env_remove("all_proxy")
                .env("HTTPS_PROXY", PROXY)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed;"),
                "proxy test subprocess did not pass: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        run_query(
            Endpoint::Credits,
            true,
            false,
            false,
            stub_key,
            |agent, request| {
                let proxy = agent
                    .config()
                    .proxy()
                    .expect("HTTPS_PROXY must be preserved");
                assert_eq!(proxy.uri(), PROXY);
                assert_eq!(request.uri(), "https://openrouter.ai/api/v1/credits");
                Ok(Response::new(ureq::Body::builder().data(MOCK_CREDITS)))
            },
        )
        .unwrap();
    }

    #[test]
    fn credits_query_pins_endpoint_and_preserves_human_and_json_output() {
        let url = "https://openrouter.ai/api/v1/credits";
        let query = |json| {
            run_query(
                Endpoint::Credits,
                json,
                false,
                false,
                stub_key,
                mock_response(url, MOCK_CREDITS),
            )
            .unwrap()
        };
        assert_eq!(
            query(false),
            (
                "$2.00 remaining  ($8.00 used of $10.00)".to_owned(),
                Some("Low - top up at https://openrouter.ai/credits".to_owned())
            )
        );
        assert_eq!(
            query(true),
            (
                "{\n  \"remaining\": 2.0,\n  \"used\": 8.0,\n  \"total\": 10.0\n}".to_owned(),
                None
            )
        );
    }

    #[test]
    fn usage_query_pins_endpoint_and_preserves_human_and_json_output() {
        let url = "https://openrouter.ai/api/v1/auth/key";
        let body = r#"{"data":{"usage_daily":0.25,"usage_weekly":1.5,"usage_monthly":8,"byok_usage_daily":0.5,"byok_usage_weekly":1,"byok_usage_monthly":2}}"#;
        let query = |json| {
            run_query(
                Endpoint::Usage,
                json,
                false,
                false,
                stub_key,
                mock_response(url, body),
            )
            .unwrap()
        };
        assert_eq!(
            query(false),
            (
                "Daily:   $0.75\nWeekly:  $2.50\nMonthly: $10.00".to_owned(),
                None
            )
        );
        assert_eq!(
            query(true),
            (
                "{\n  \"daily\": 0.75,\n  \"weekly\": 2.5,\n  \"monthly\": 10.0\n}".to_owned(),
                None
            )
        );
    }

    #[test]
    fn override_is_rejected_before_credentials_or_transport() {
        for endpoint in [Endpoint::Credits, Endpoint::Usage] {
            let error = run_query(
                endpoint,
                false,
                false,
                true,
                || panic!("looked up credentials with an endpoint override"),
                |_, _| panic!("called transport with an endpoint override"),
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Error: OPENROUTER_BASE_URL is no longer supported. Unset it to use OpenRouter."
            );
        }
    }

    #[test]
    fn redirects_are_disabled_and_3xx_responses_are_rejected() {
        for (status, target) in [
            (302, "https://example.invalid/"),
            (307, "https://openrouter.ai/"),
        ] {
            let error = run_query(
                Endpoint::Credits,
                true,
                false,
                false,
                stub_key,
                move |agent, _| {
                    assert_eq!(agent.config().max_redirects(), 0);
                    Ok(Response::builder()
                        .status(status)
                        .header(header::LOCATION, format!("{target}{SYNTHETIC_KEY}"))
                        .body(ureq::Body::builder().data(MOCK_CREDITS))
                        .unwrap())
                },
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Error: OpenRouter HTTP redirect refused."
            );
        }
    }

    #[test]
    fn requests_have_a_global_timeout_and_redact_timeout_errors() {
        let error = run_query(Endpoint::Usage, true, false, false, stub_key, |agent, _| {
            assert_eq!(
                agent.config().timeouts().global,
                Some(Duration::from_secs(12))
            );
            assert_eq!(
                agent.config().timeouts().connect,
                Some(Duration::from_secs(5))
            );
            Err(ureq::Error::Timeout(ureq::Timeout::Global))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "Error: OpenRouter request timed out.");

        struct TimedOutBody;
        impl io::Read for TimedOutBody {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::TimedOut, SYNTHETIC_KEY))
            }
        }
        let error = run_query(Endpoint::Credits, true, false, false, stub_key, |_, _| {
            Ok(Response::new(ureq::Body::builder().reader(TimedOutBody)))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "Error: OpenRouter request timed out.");
    }

    #[test]
    fn transport_status_and_json_errors_do_not_echo_credentials() {
        let error = run_query(Endpoint::Credits, false, false, false, stub_key, |_, _| {
            Err(ureq::Error::Io(io::Error::other(SYNTHETIC_KEY)))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "Error: OpenRouter request failed.");

        for (status, diagnostic) in [
            (200, "Error: Invalid OpenRouter response."),
            (401, "Error: OpenRouter returned HTTP 401."),
        ] {
            let error = run_query(
                Endpoint::Credits,
                false,
                false,
                false,
                stub_key,
                move |_, _| {
                    Ok(Response::builder()
                        .status(status)
                        .body(
                            ureq::Body::builder().data(format!(r#"{{"data":"{SYNTHETIC_KEY}"}}"#)),
                        )
                        .unwrap())
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), diagnostic);
        }
    }

    #[test]
    fn invalid_bearer_header_is_rejected_without_echo_or_transport() {
        let error = run_query(
            Endpoint::Usage,
            true,
            false,
            false,
            || Ok(format!("{SYNTHETIC_KEY}\n")),
            |_, _| panic!("sent an invalid authorization header"),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Error: Invalid API key format.");
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
    fn query_with_missing_key_gives_manual_setup_guidance_without_transport() {
        let error = run_query(
            Endpoint::Credits,
            false,
            false,
            false,
            || Err(AppError::ApiKeyMissing),
            |_, _| panic!("called transport without a key"),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Error: API key not found. Set OPENROUTER_API_KEY or use macOS Keychain Access \
             (service: openrouter-api-key, account: openrouter)."
        );
    }
}
