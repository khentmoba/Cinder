//! Live quota windows fetched from the providers' own APIs.
//!
//! Cinder reads local logs for usage, but the rate-limit windows a plan
//! enforces are only known to the provider. These endpoints answer with the
//! current utilisation of every window, using the OAuth token the agent
//! already stored on disk. Nothing leaves the machine except the request to
//! the provider itself, and a missing credential is simply skipped.
//!
//! The fetcher is built to survive a bad day rather than a good one:
//!
//! - a request that fails is tried again, with a backoff, before it is
//!   called a failure;
//! - a provider that keeps failing is benched for a while instead of being
//!   asked again every few minutes;
//! - the last windows that did come back are kept and handed over marked as
//!   stale, so a flaky network never blanks the panel;
//! - the snapshot is written to disk, so a launch shows the last known state
//!   instead of an empty tray while the first refresh is still in flight.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------- data model

/// One live quota window reported by a provider.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LiveQuota {
    pub agent: String,
    /// Short window id: "5h", "7d", "7d-sonnet", "monthly", "premium".
    pub window: String,
    pub used_percent: f64,
    pub window_minutes: i64,
    /// Unix seconds; 0 when the provider did not say.
    pub resets_at: i64,
    pub plan: String,
    /// Human description, e.g. "5-hour session window".
    pub detail: String,
    /// How much of the allowance is gone, in dollars, when the provider counts
    /// its allowance in money rather than in requests.
    #[serde(default)]
    pub used_amount: f64,
    /// The size of that allowance, in dollars.
    #[serde(default)]
    pub limit_amount: f64,
}

/// Why a provider could not be read.
///
/// The distinction matters: a provider that is not signed in is not a problem,
/// while a provider whose token was refused is a problem the user can fix.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The agent is not signed in on this machine.
    NotSignedIn,
    /// The stored credential was refused: expired, revoked, or wrong.
    Unauthorized,
    /// The provider could not be reached.
    Unreachable,
    /// The provider took too long to answer.
    Timeout,
    /// The provider answered with an error status.
    Status,
    /// The answer did not look like a usage payload.
    BadPayload,
}

/// What one provider reported.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuotaReport {
    pub agent: String,
    pub windows: Vec<LiveQuota>,
    /// Why the provider could not be read. Empty on success.
    pub error: String,
    /// True when the provider is simply not signed in on this machine, which
    /// is not a failure worth telling the user about.
    pub skipped: bool,
    /// True when these windows are the last ones that worked and the newest
    /// refresh did not come back.
    #[serde(default)]
    pub stale: bool,
    /// Unix seconds of the last successful read for this provider.
    #[serde(default)]
    pub updated_at: i64,
    /// Unix seconds of the last failed attempt.
    #[serde(default)]
    pub failed_at: i64,
    /// How many times in a row the provider has failed.
    #[serde(default)]
    pub failures: u32,
    /// Why the provider could not be read.
    #[serde(default)]
    pub error_kind: Option<ErrorKind>,
    /// The plan the account is on, e.g. "max", "pro", "plus".
    #[serde(default)]
    pub plan: String,
    /// Which account this is about, when a machine holds more than one.
    #[serde(default)]
    pub account: String,
}

impl QuotaReport {
    pub fn ok(agent: &str, windows: Vec<LiveQuota>) -> Self {
        Self {
            agent: agent.to_string(),
            windows,
            error: String::new(),
            skipped: false,
            stale: false,
            updated_at: 0,
            failed_at: 0,
            failures: 0,
            error_kind: None,
            plan: String::new(),
            account: String::new(),
        }
    }
    pub fn failed(agent: &str, kind: ErrorKind, error: impl Into<String>) -> Self {
        Self {
            agent: agent.to_string(),
            windows: Vec::new(),
            error: error.into(),
            skipped: false,
            stale: false,
            updated_at: 0,
            failed_at: 0,
            failures: 0,
            error_kind: Some(kind),
            plan: String::new(),
            account: String::new(),
        }
    }
    /// The agent is not signed in here, so there is nothing to report.
    pub fn absent(agent: &str) -> Self {
        Self {
            agent: agent.to_string(),
            windows: Vec::new(),
            error: String::new(),
            skipped: true,
            stale: false,
            updated_at: 0,
            failed_at: 0,
            failures: 0,
            error_kind: Some(ErrorKind::NotSignedIn),
            plan: String::new(),
            account: String::new(),
        }
    }
}

// ----------------------------------------------------------------- constants

/// How long a single provider request may take before it is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// How long one provider may take in total, retries included.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(45);
/// How often the background refresher asks the providers again.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How many times a request is attempted before it is called a failure.
const ATTEMPTS: u32 = 3;
/// The wait before the second attempt. It doubles from there.
const RETRY_BASE: Duration = Duration::from_millis(400);
/// The version of the on-disk snapshot, so an old file is not misread.
const CACHE_VERSION: u32 = 1;

/// The public client id Claude Code registers with Anthropic.
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// The public client id the Codex CLI registers with OpenAI.
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Where the providers live.
///
/// The endpoints are data rather than constants so the tests can point the
/// fetcher at a local server instead of the internet.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub claude_usage: String,
    pub claude_token: String,
    pub codex_usage: String,
    pub codex_plan: String,
    pub codex_token: String,
    pub copilot_usage: String,
    pub gemini_usage: String,
    pub opencode_usage: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            claude_usage: "https://api.anthropic.com/api/oauth/usage".into(),
            claude_token: "https://console.anthropic.com/v1/oauth/token".into(),
            codex_usage: "https://chatgpt.com/backend-api/wham/usage".into(),
            codex_plan: "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits".into(),
            codex_token: "https://auth.openai.com/oauth/token".into(),
            copilot_usage: "https://api.github.com/copilot_internal/user".into(),
            gemini_usage: "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist".into(),
            opencode_usage: "https://opencode.ai/api/usage".into(),
        }
    }
}

// ---------------------------------------------------------------- credentials

/// One signed-in account: where it lives, what it can be called, and how to
/// keep it alive.
#[derive(Clone, Debug)]
struct Credential {
    /// Where the credentials are stored.
    path: PathBuf,
    /// The token to send.
    token: String,
    /// The refresh token, when the agent stored one.
    refresh: Option<String>,
    /// When the token stops working, in unix seconds. 0 when unknown.
    expires_at: i64,
    /// The plan the account is on, when the agent said.
    plan: String,
    /// Who the account belongs to, when the token says.
    account: String,
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write a JSON file without leaving a half-written one behind.
fn write_json_atomic(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(value).unwrap_or_default())?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// First non-empty string found under any of `keys` in `v`.
fn str_at(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// The claims inside a JWT, without checking the signature.
///
/// The signature is the provider's business; the claims are only read to
/// name an account and to see when its token runs out.
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let mut bytes = Vec::new();
    // base64url: the alphabet differs from standard base64 and there is no
    // padding at the end.
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in payload.bytes() {
        let Some(v) = TABLE.iter().position(|t| *t == c) else { continue };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
        }
    }
    serde_json::from_slice(&bytes).ok()
}

/// The account a token belongs to, and when it runs out.
fn token_identity(token: &str) -> (String, i64) {
    let Some(claims) = jwt_claims(token) else { return (String::new(), 0) };
    let account = str_at(&claims, &["email", "org_id", "organization_id", "account_id"])
        .map(|a| display_account(&a))
        .unwrap_or_default();
    let expires_at = claims.get("exp").and_then(|x| x.as_i64()).unwrap_or(0);
    (account, expires_at)
}

/// A name for an account that is worth putting on screen.
///
/// An email is used as it is; an opaque id is shortened, because a full uuid
/// in a card title helps nobody.
fn display_account(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.contains('@') {
        return raw.to_string();
    }
    if raw.chars().count() > 12 {
        return raw.chars().take(8).collect();
    }
    raw.to_string()
}

/// The directories an agent may keep its credentials in.
///
/// A second login lives in its own folder (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`),
/// and each of those is a separate subscription with its own limits.
fn config_dirs(home: &Path, env: &str, default: &str) -> Vec<PathBuf> {
    let mut dirs = vec![home.join(default)];
    if let Ok(dir) = std::env::var(env) {
        if !dir.trim().is_empty() {
            let dir = PathBuf::from(dir);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// The Claude Code accounts signed in on this machine.
fn claude_credentials(home: &Path) -> Vec<Credential> {
    let mut out = Vec::new();
    for dir in config_dirs(home, "CLAUDE_CONFIG_DIR", ".claude") {
        let path = dir.join(".credentials.json");
        let Some(v) = read_json(&path) else { continue };
        let Some(o) = v.get("claudeAiOauth") else { continue };
        let Some(token) = o.get("accessToken").and_then(|x| x.as_str()) else { continue };
        if token.trim().is_empty() {
            continue;
        }
        let (mut account, mut expires_at) = token_identity(token);
        if account.is_empty() {
            account = str_at(o, &["organizationName", "organization_name"])
                .map(|a| display_account(&a))
                .unwrap_or_default();
        }
        if expires_at == 0 {
            // Anthropic stores the expiry in milliseconds.
            expires_at = o.get("expiresAt").and_then(|x| x.as_i64()).map(|ms| ms / 1000).unwrap_or(0);
        }
        out.push(Credential {
            path,
            token: token.to_string(),
            refresh: o.get("refreshToken").and_then(|x| x.as_str()).map(|s| s.to_string()),
            expires_at,
            plan: str_at(o, &["subscriptionType", "subscription_type"]).unwrap_or_default(),
            account,
        });
    }
    out
}

/// The ChatGPT accounts Codex is signed in with on this machine.
fn codex_credentials(home: &Path) -> Vec<Credential> {
    let mut out = Vec::new();
    for dir in config_dirs(home, "CODEX_HOME", ".codex") {
        let path = dir.join("auth.json");
        let Some(v) = read_json(&path) else { continue };
        let Some(t) = v.get("tokens") else { continue };
        let Some(token) = t.get("access_token").and_then(|x| x.as_str()) else { continue };
        if token.trim().is_empty() {
            continue;
        }
        let (mut account, expires_at) = token_identity(token);
        if account.is_empty() {
            account = str_at(t, &["account_id"]).map(|a| display_account(&a)).unwrap_or_default();
        }
        out.push(Credential {
            path,
            token: token.to_string(),
            refresh: t.get("refresh_token").and_then(|x| x.as_str()).map(|s| s.to_string()),
            expires_at,
            plan: String::new(),
            account,
        });
    }
    out
}

/// The GitHub token the Copilot extension stores.
fn copilot_token(home: &Path) -> Option<String> {
    let mut candidates = vec![
        home.join(".config").join("github-copilot").join("apps.json"),
        home.join(".config").join("github-copilot").join("hosts.json"),
    ];
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.trim().is_empty() {
            let base = PathBuf::from(appdata).join("github-copilot");
            candidates.push(base.join("apps.json"));
            candidates.push(base.join("hosts.json"));
        }
    }
    for path in candidates {
        let Some(v) = read_json(&path) else { continue };
        // Both shapes are maps of account id -> credential object.
        let Some(accounts) = v.as_object() else { continue };
        for (_id, cred) in accounts {
            if let Some(t) = str_at(cred, &["oauth_token", "token", "access_token"]) {
                return Some(t);
            }
        }
    }
    None
}

/// The Google access token the Gemini CLI stores.
fn gemini_token(home: &Path) -> Option<String> {
    for path in [home.join(".gemini").join("oauth_creds.json")] {
        let Some(v) = read_json(&path) else { continue };
        if let Some(t) = str_at(&v, &["access_token", "accessToken"]) {
            return Some(t);
        }
    }
    None
}

/// The OpenCode API key, if the user configured one.
fn opencode_token(home: &Path) -> Option<String> {
    if let Ok(t) = std::env::var("OPENCODE_API_KEY") {
        if !t.trim().is_empty() {
            return Some(t);
        }
    }
    for path in [
        home.join(".local").join("share").join("opencode").join("auth.json"),
        home.join(".config").join("opencode").join("auth.json"),
    ] {
        let Some(v) = read_json(&path) else { continue };
        if let Some(t) = str_at(&v, &["opencode", "apiKey", "api_key", "token"]) {
            return Some(t);
        }
    }
    None
}

// ------------------------------------------------------------ token refreshing

/// Exchange a refresh token for a fresh access token.
///
/// The agents themselves refresh these tokens, but only when they are run. A
/// tracker that runs all day would otherwise see an expired token and report
/// a problem that is not there, so the refresh is done here as well and
/// written back — which also keeps the agent signed in.
async fn refresh_token(
    client: &reqwest::Client,
    endpoint: &str,
    client_id: &str,
    refresh: &str,
) -> Option<(String, Option<String>, i64)> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": client_id,
        "refresh_token": refresh,
    });
    let resp = client
        .post(endpoint)
        .header("User-Agent", "Cinder")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let access = str_at(&v, &["access_token", "accessToken"])?;
    let next_refresh = str_at(&v, &["refresh_token", "refreshToken"]);
    // The expiry comes back in seconds; the claims say the same thing.
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(0);
    let (_, claim_exp) = token_identity(&access);
    let expires_at = if claim_exp > 0 { claim_exp } else { now_secs() + expires_in };
    Some((access, next_refresh, expires_at))
}

/// Refresh a Claude Code credential and write the new token back.
async fn refresh_claude(client: &reqwest::Client, endpoints: &Endpoints, cred: &Credential) -> Option<Credential> {
    let refresh = cred.refresh.clone()?;
    let (access, next_refresh, expires_at) =
        refresh_token(client, &endpoints.claude_token, CLAUDE_CLIENT_ID, &refresh).await?;
    // Keep the file as it was, with only the token fields replaced.
    if let Some(mut v) = read_json(&cred.path) {
        if let Some(o) = v.get_mut("claudeAiOauth").and_then(|o| o.as_object_mut()) {
            o.insert("accessToken".into(), serde_json::Value::String(access.clone()));
            if let Some(r) = &next_refresh {
                o.insert("refreshToken".into(), serde_json::Value::String(r.clone()));
            }
            o.insert("expiresAt".into(), serde_json::Value::from(expires_at * 1000));
            let _ = write_json_atomic(&cred.path, &v);
        }
    }
    let mut next = cred.clone();
    next.token = access;
    if let Some(r) = next_refresh {
        next.refresh = Some(r);
    }
    next.expires_at = expires_at;
    Some(next)
}

/// Refresh a Codex credential and write the new token back.
async fn refresh_codex(client: &reqwest::Client, endpoints: &Endpoints, cred: &Credential) -> Option<Credential> {
    let refresh = cred.refresh.clone()?;
    let (access, next_refresh, expires_at) =
        refresh_token(client, &endpoints.codex_token, CODEX_CLIENT_ID, &refresh).await?;
    if let Some(mut v) = read_json(&cred.path) {
        if let Some(t) = v.get_mut("tokens").and_then(|t| t.as_object_mut()) {
            t.insert("access_token".into(), serde_json::Value::String(access.clone()));
            if let Some(r) = &next_refresh {
                t.insert("refresh_token".into(), serde_json::Value::String(r.clone()));
            }
            let _ = write_json_atomic(&cred.path, &v);
        }
    }
    let mut next = cred.clone();
    next.token = access;
    if let Some(r) = next_refresh {
        next.refresh = Some(r);
    }
    next.expires_at = expires_at;
    Some(next)
}

// ------------------------------------------------------------------- parsing

/// A reset time is either unix seconds or an ISO-8601 timestamp.
fn parse_resets_at(v: &serde_json::Value) -> i64 {
    if let Some(n) = v.as_i64() {
        return if n > 1_000_000_000_000 { n / 1000 } else { n };
    }
    let Some(s) = v.as_str() else { return 0 };
    if let Ok(n) = s.parse::<i64>() {
        return if n > 1_000_000_000_000 { n / 1000 } else { n };
    }
    parse_iso(s)
}

/// An ISO-8601 timestamp to unix seconds.
///
/// The time of day matters: a window that resets at 18:00 is not a window
/// that resets at midnight, and getting that wrong puts the reset countdown
/// hours off.
fn parse_iso(s: &str) -> i64 {
    let s = s.trim();
    if s.len() < 10 {
        return 0;
    }
    let (y, m, d) = parse_day(&s[..10]);
    let mut secs = days_from_civil(y, m, d) * 86_400;
    let rest = &s[10..];
    let rest = rest.trim_start_matches(['T', ' ']);
    if rest.len() >= 5 {
        let hour: i64 = rest[0..2].parse().unwrap_or(0);
        let minute: i64 = rest[3..5].parse().unwrap_or(0);
        secs += hour * 3600 + minute * 60;
        if rest.len() >= 8 {
            if let Ok(second) = rest[6..8].parse::<i64>() {
                secs += second;
            }
        }
    }
    // A numeric offset shifts the clock; "Z" means none.
    if let Some(at) = rest.find(['+', '-']) {
        let sign = if rest.as_bytes()[at] == b'-' { -1 } else { 1 };
        let off = &rest[at + 1..];
        if off.len() >= 4 {
            let oh: i64 = off[0..2].parse().unwrap_or(0);
            let om: i64 = off[3..5].parse().unwrap_or(0);
            secs -= sign * (oh * 3600 + om * 60);
        }
    }
    secs
}

/// "2026-10-21" -> (2026, 10, 21).
fn parse_day(day: &str) -> (i64, i64, i64) {
    let mut it = day.split('-');
    let y = it.next().and_then(|x| x.parse().ok()).unwrap_or(1970);
    let m = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    let d = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
    (y, m, d)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Utilisation as a percentage, accepting 0..100 and 0..1 shapes.
fn percent(v: &serde_json::Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n.clamp(0.0, 100.0));
    }
    let used = v.get("used")?.as_f64()?;
    let total = v.get("total")?.as_f64()?;
    if total <= 0.0 {
        return None;
    }
    Some((used / total * 100.0).clamp(0.0, 100.0))
}

/// Claude's usage payload: five_hour, seven_day, seven_day_sonnet, ...
fn parse_claude_usage(v: &serde_json::Value, plan: &str) -> Vec<LiveQuota> {
    let mut out = Vec::new();
    let windows: [(&str, &str, i64, &str); 5] = [
        ("five_hour", "5h", 300, "5-hour session window"),
        ("seven_day", "7d", 7 * 24 * 60, "7-day window"),
        ("seven_day_sonnet", "7d-sonnet", 7 * 24 * 60, "Weekly Sonnet limit"),
        ("seven_day_opus", "7d-opus", 7 * 24 * 60, "Weekly Opus limit"),
        ("seven_day_haiku", "7d-haiku", 7 * 24 * 60, "Weekly Haiku limit"),
    ];
    for (key, id, minutes, detail) in windows {
        let Some(w) = v.get(key) else { continue };
        let Some(pct) = w.get("utilization").and_then(percent) else { continue };
        out.push(LiveQuota {
            agent: "Claude Code".into(),
            window: id.into(),
            used_percent: pct,
            window_minutes: minutes,
            resets_at: w.get("resets_at").map(parse_resets_at).unwrap_or(0),
            plan: plan.into(),
            detail: detail.into(),
            ..Default::default()
        });
    }
    // Extra usage is a credit balance, not a window: report how much of the
    // monthly allowance is gone.
    if let Some(extra) = v.get("extra_usage") {
        let used = extra.get("used_credits").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let limit = extra.get("monthly_limit").and_then(|x| x.as_f64()).unwrap_or(0.0);
        if limit > 0.0 {
            out.push(LiveQuota {
                agent: "Claude Code".into(),
                window: "monthly".into(),
                used_percent: (used / limit * 100.0).clamp(0.0, 100.0),
                window_minutes: 30 * 24 * 60,
                resets_at: 0,
                plan: plan.into(),
                detail: "Extra usage credits".into(),
                used_amount: used,
                limit_amount: limit,
            });
        }
    }
    out
}

/// Codex's usage payload.
///
/// The endpoint has changed shape more than once, so rather than expect one
/// layout the payload is searched for the objects that carry a percentage:
/// any object with a `used_percent` is a window, and the name it sits under
/// says which window it is.
fn parse_codex_usage(v: &serde_json::Value, plan: &str) -> Vec<LiveQuota> {
    let mut out = Vec::new();
    collect_windows(v, "Codex", plan, &mut Vec::new(), &mut out);
    out
}

/// The name of a Codex window, and how long it is when the payload does not
/// say.
fn codex_window_name(key: &str) -> Option<(&'static str, i64, &'static str)> {
    let key = key.to_lowercase();
    if key.contains("primary") || key.contains("five_hour") || key.contains("5h") {
        return Some(("5h", 300, "5-hour session window"));
    }
    if key.contains("secondary") || key.contains("weekly") || key.contains("7d") {
        return Some(("7d", 7 * 24 * 60, "7-day window"));
    }
    None
}

/// Walk a usage payload and collect every window it holds.
fn collect_windows(
    v: &serde_json::Value,
    agent: &str,
    plan: &str,
    path: &mut Vec<String>,
    out: &mut Vec<LiveQuota>,
) {
    let now = now_secs();
    let Some(obj) = v.as_object() else { return };
    // An object that says how much of a window is used is a window.
    let pct = obj
        .get("used_percent")
        .or_else(|| obj.get("utilization"))
        .or_else(|| obj.get("percent_used"))
        .and_then(percent);
    if let Some(pct) = pct {
        let key = path.last().cloned().unwrap_or_default();
        let (id, default_minutes, detail) = codex_window_name(&key)
            .unwrap_or(("window", 0, "Rate limit window"));
        // The window length is sometimes given in seconds, and the reset
        // sometimes as a countdown rather than a moment.
        let minutes = obj
            .get("window_minutes")
            .and_then(|x| x.as_i64())
            .or_else(|| {
                obj.get("limit_window_seconds")
                    .and_then(|x| x.as_i64())
                    .map(|s| s / 60)
            })
            .unwrap_or(default_minutes);
        let resets_at = obj
            .get("reset_at")
            .or_else(|| obj.get("resets_at"))
            .map(parse_resets_at)
            .filter(|t| *t > 0)
            .or_else(|| {
                obj.get("reset_after_seconds")
                    .and_then(|x| x.as_i64())
                    .map(|s| now + s)
            })
            .unwrap_or(0);
        out.push(LiveQuota {
            agent: agent.into(),
            window: id.into(),
            used_percent: pct,
            window_minutes: minutes,
            resets_at,
            plan: plan.into(),
            detail: detail.into(),
            ..Default::default()
        });
        return;
    }
    for (k, child) in obj {
        path.push(k.clone());
        collect_windows(child, agent, plan, path, out);
        path.pop();
    }
}

/// Copilot's user payload: premium interactions and the chat buckets.
fn parse_copilot_usage(v: &serde_json::Value) -> Vec<LiveQuota> {
    let mut out = Vec::new();
    if let Some(snap) = v.get("quota_snapshots").and_then(|q| q.get("premium_interactions")) {
        let entitlement = snap.get("entitlement").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let remaining = snap.get("remaining").and_then(|x| x.as_f64()).unwrap_or(0.0);
        if entitlement > 0.0 {
            out.push(LiveQuota {
                agent: "GitHub Copilot".into(),
                window: "premium".into(),
                used_percent: ((entitlement - remaining) / entitlement * 100.0).clamp(0.0, 100.0),
                window_minutes: 30 * 24 * 60,
                resets_at: 0,
                plan: "copilot".into(),
                detail: "Premium interactions".into(),
                ..Default::default()
            });
        }
    }
    if let Some(quotas) = v.get("limited_user_quotas").and_then(|q| q.as_object()) {
        for (name, q) in quotas {
            let Some(entitlement) = q.get("entitlement").and_then(|x| x.as_f64()) else { continue };
            let remaining = q.get("remaining").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if entitlement <= 0.0 {
                continue;
            }
            out.push(LiveQuota {
                agent: "GitHub Copilot".into(),
                window: name.clone(),
                used_percent: ((entitlement - remaining) / entitlement * 100.0).clamp(0.0, 100.0),
                window_minutes: 30 * 24 * 60,
                resets_at: 0,
                plan: "copilot".into(),
                detail: format!("{} quota", name),
                ..Default::default()
            });
        }
    }
    out
}

/// Gemini CLI's loadCodeAssist payload: a list of quota buckets.
fn parse_gemini_usage(v: &serde_json::Value) -> Vec<LiveQuota> {
    let plan = v
        .get("currentTier")
        .and_then(|t| t.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("free-tier")
        .to_string();
    let buckets = v
        .get("quotaBuckets")
        .or_else(|| v.get("quotaBuckets"))
        .and_then(|b| b.as_array());
    let Some(buckets) = buckets else { return Vec::new() };
    let mut out = Vec::new();
    for b in buckets {
        let id = b.get("quotaId").and_then(|x| x.as_str()).unwrap_or("quota");
        let consumed = b.get("quotaConsumed").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let limit = b.get("quotaLimit").and_then(|x| x.as_f64()).unwrap_or(0.0);
        if limit <= 0.0 {
            continue;
        }
        out.push(LiveQuota {
            agent: "Gemini CLI".into(),
            window: id.rsplit('/').next().unwrap_or(id).to_string(),
            used_percent: (consumed / limit * 100.0).clamp(0.0, 100.0),
            window_minutes: 24 * 60,
            resets_at: 0,
            plan: plan.clone(),
            detail: id.to_string(),
            ..Default::default()
        });
    }
    out
}

/// OpenCode's usage payload: rolling five-hour, weekly and monthly windows.
fn parse_opencode_usage(v: &serde_json::Value) -> Vec<LiveQuota> {
    let mut out = Vec::new();
    let windows: [(&str, &str, i64, &str); 3] = [
        ("five_hour", "5h", 300, "5-hour session window"),
        ("weekly", "7d", 7 * 24 * 60, "7-day window"),
        ("monthly", "30d", 30 * 24 * 60, "30-day window"),
    ];
    for (key, id, minutes, detail) in windows {
        let Some(w) = v.get(key) else { continue };
        let Some(pct) = w.get("utilization").and_then(percent).or_else(|| w.as_f64().map(|n| n.clamp(0.0, 100.0))) else {
            continue;
        };
        out.push(LiveQuota {
            agent: "OpenCode".into(),
            window: id.into(),
            used_percent: pct,
            window_minutes: minutes,
            resets_at: w.get("resets_at").map(parse_resets_at).unwrap_or(0),
            plan: String::new(),
            detail: detail.into(),
            ..Default::default()
        });
    }
    out
}

// ------------------------------------------------------------------ fetching

/// Why one request did not produce a payload.
#[derive(Clone, Debug, PartialEq)]
enum FetchError {
    /// The provider could not be reached.
    Network(String),
    /// The provider took too long.
    Timeout(String),
    /// The provider answered with an error status.
    Status(u16),
    /// The answer was not JSON.
    BadJson(String),
}

impl FetchError {
    /// Whether trying again could plausibly work.
    fn retryable(&self) -> bool {
        match self {
            FetchError::Network(_) | FetchError::Timeout(_) => true,
            // A server-side error or a rate limit is worth another attempt; a
            // refused token is not.
            FetchError::Status(code) => *code == 429 || *code >= 500,
            FetchError::BadJson(_) => false,
        }
    }

    /// The kind the user sees.
    fn kind(&self) -> ErrorKind {
        match self {
            FetchError::Network(_) => ErrorKind::Unreachable,
            FetchError::Timeout(_) => ErrorKind::Timeout,
            FetchError::Status(401) | FetchError::Status(403) => ErrorKind::Unauthorized,
            FetchError::Status(_) => ErrorKind::Status,
            FetchError::BadJson(_) => ErrorKind::BadPayload,
        }
    }

    fn message(&self) -> String {
        match self {
            FetchError::Network(m) => m.clone(),
            FetchError::Timeout(m) => m.clone(),
            FetchError::Status(code) => format!("HTTP {code}"),
            FetchError::BadJson(m) => m.clone(),
        }
    }
}

/// One attempt at a request.
async fn try_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    extra: &[(&str, &str)],
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, FetchError> {
    let mut req = client
        .request(method, url)
        .bearer_auth(token)
        .header("User-Agent", "Cinder")
        .header("Accept", "application/json");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            FetchError::Timeout(e.to_string())
        } else {
            FetchError::Network(e.to_string())
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    resp.json::<serde_json::Value>().await.map_err(|e| FetchError::BadJson(e.to_string()))
}

/// A request that is tried again when it fails for a reason that might pass.
async fn get_json(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    extra: &[(&str, &str)],
) -> Result<serde_json::Value, FetchError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = try_json(client, reqwest::Method::GET, url, token, extra, None).await;
        match result {
            Ok(v) => return Ok(v),
            Err(e) if e.retryable() && attempt < ATTEMPTS => {
                // The wait doubles each time, with a little jitter so two
                // providers failing together do not retry in lockstep.
                let backoff = RETRY_BASE * 2u32.pow(attempt - 1);
                let jitter = (attempt as u64 * 37) % 120;
                tokio::time::sleep(backoff + Duration::from_millis(jitter)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The name a provider is shown under.
///
/// One account keeps the plain name, so the alerts and the tray read the way
/// they always did. Several accounts are told apart by who they belong to.
fn account_label(agent: &str, cred: &Credential, many: bool) -> String {
    if !many || cred.account.is_empty() {
        return agent.to_string();
    }
    format!("{agent} \u{b7} {}", cred.account)
}

/// The Claude Code usage for one account.
async fn claude_code(client: reqwest::Client, endpoints: Endpoints, cred: Credential, many: bool) -> QuotaReport {
    let label = account_label("Claude Code", &cred, many);
    let v = match get_json(
        &client,
        &endpoints.claude_usage,
        &cred.token,
        &[("anthropic-beta", "oauth-2025-04-20")],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed(&label, e.kind(), e.message()),
    };
    let mut plan = v
        .get("plan")
        .or_else(|| v.get("subscription_plan"))
        .or_else(|| v.get("plan_type"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if plan.is_empty() {
        plan = cred.plan.clone();
    }
    let mut windows = parse_claude_usage(&v, &plan);
    if windows.is_empty() {
        return QuotaReport::failed(&label, ErrorKind::BadPayload, "unexpected usage payload");
    }
    for w in windows.iter_mut() {
        w.agent = label.clone();
    }
    let mut report = QuotaReport::ok(&label, windows);
    report.plan = plan;
    report.account = cred.account.clone();
    report.updated_at = now_secs();
    report
}

/// The Codex usage for one account.
async fn codex(client: reqwest::Client, endpoints: Endpoints, cred: Credential, many: bool) -> QuotaReport {
    let label = account_label("Codex", &cred, many);
    let usage = get_json(
        &client,
        &endpoints.codex_usage,
        &cred.token,
        &[("OpenAI-Beta", "codex-1")],
    )
    .await;
    let Ok(usage) = usage else {
        let e = usage.unwrap_err();
        return QuotaReport::failed(&label, e.kind(), e.message());
    };
    // The plan type is in the usage payload itself; the second endpoint is
    // only a fallback.
    let plan = match usage.get("plan_type").and_then(|x| x.as_str()) {
        Some(p) => p.to_string(),
        None => get_json(
            &client,
            &endpoints.codex_plan,
            &cred.token,
            &[("OpenAI-Beta", "codex-1")],
        )
        .await
        .ok()
        .and_then(|v| {
            v.get("plan_type")
                .or_else(|| v.get("rate_limit_resets").and_then(|r| r.get("plan_type")))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default(),
    };
    let mut windows = parse_codex_usage(&usage, &plan);
    if windows.is_empty() {
        return QuotaReport::failed(&label, ErrorKind::BadPayload, "unexpected usage payload");
    }
    for w in windows.iter_mut() {
        w.agent = label.clone();
    }
    let mut report = QuotaReport::ok(&label, windows);
    report.plan = plan;
    report.account = cred.account.clone();
    report.updated_at = now_secs();
    report
}

/// The Copilot usage.
async fn copilot(client: reqwest::Client, endpoints: Endpoints, home: PathBuf) -> QuotaReport {
    let Some(token) = copilot_token(&home) else {
        return QuotaReport::absent("GitHub Copilot");
    };
    let v = match get_json(
        &client,
        &endpoints.copilot_usage,
        &token,
        &[
            ("Accept", "application/vnd.github+json"),
            ("Copilot-Integration-Id", "vscode-chat"),
        ],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("GitHub Copilot", e.kind(), e.message()),
    };
    let windows = parse_copilot_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("GitHub Copilot", ErrorKind::BadPayload, "unexpected usage payload");
    }
    let mut report = QuotaReport::ok("GitHub Copilot", windows);
    report.plan = "copilot".into();
    report.updated_at = now_secs();
    report
}

/// The Gemini CLI usage.
async fn gemini_cli(client: reqwest::Client, endpoints: Endpoints, home: PathBuf) -> QuotaReport {
    let Some(token) = gemini_token(&home) else {
        return QuotaReport::absent("Gemini CLI");
    };
    let v = match get_json(
        &client,
        &endpoints.gemini_usage,
        &token,
        &[("Content-Type", "application/json")],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("Gemini CLI", e.kind(), e.message()),
    };
    let windows = parse_gemini_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("Gemini CLI", ErrorKind::BadPayload, "unexpected usage payload");
    }
    let plan = windows.first().map(|w| w.plan.clone()).unwrap_or_default();
    let mut report = QuotaReport::ok("Gemini CLI", windows);
    report.plan = plan;
    report.updated_at = now_secs();
    report
}

/// The OpenCode usage.
async fn opencode(client: reqwest::Client, endpoints: Endpoints, home: PathBuf) -> QuotaReport {
    let Some(token) = opencode_token(&home) else {
        return QuotaReport::absent("OpenCode");
    };
    let v = match get_json(&client, &endpoints.opencode_usage, &token, &[]).await {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("OpenCode", e.kind(), e.message()),
    };
    let windows = parse_opencode_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("OpenCode", ErrorKind::BadPayload, "unexpected usage payload");
    }
    let mut report = QuotaReport::ok("OpenCode", windows);
    report.updated_at = now_secs();
    report
}
// ------------------------------------------------------------- provider state

/// What is known about one provider between refreshes.
#[derive(Default)]
struct ProviderState {
    /// The last windows that came back, and when they did.
    good: Option<(Vec<LiveQuota>, i64)>,
    /// When the provider may be asked again after a failure.
    retry_after: i64,
    /// How many failures in a row.
    failures: u32,
    /// The last error text.
    error: String,
    /// The kind of the last error.
    kind: Option<ErrorKind>,
}

static STATE: LazyLock<Mutex<HashMap<String, ProviderState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How long a provider is benched after failing, by the number of failures.
///
/// The first failure costs a minute, the last one a quarter of an hour: a
/// provider that is down should be left alone, and one that recovers should
/// not wait long for that.
fn backoff_seconds(failures: u32) -> i64 {
    match failures {
        0 => 0,
        1 => 60,
        2 => 120,
        3 => 300,
        _ => 900,
    }
}

/// The result of one provider attempt, with the stale fallback applied.
fn settle(report: QuotaReport, now: i64) -> QuotaReport {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let entry = state.entry(report.agent.clone()).or_default();
    if report.error.is_empty() {
        entry.good = Some((report.windows.clone(), now));
        entry.retry_after = 0;
        entry.failures = 0;
        entry.error.clear();
        entry.kind = None;
        return report;
    }
    entry.failures = entry.failures.saturating_add(1);
    entry.retry_after = now + backoff_seconds(entry.failures);
    entry.error = report.error.clone();
    entry.kind = report.error_kind;
    let mut out = report;
    out.failed_at = now;
    out.failures = entry.failures;
    // The last windows that did come back are better than nothing, as long as
    // they are marked as the numbers they are: from the last good read.
    if let Some((windows, at)) = &entry.good {
        if !windows.is_empty() {
            out.windows = windows.clone();
            out.stale = true;
            out.updated_at = *at;
        }
    }
    out
}

/// The report for a provider that is being left alone after failing.
fn benched(agent: &str, now: i64) -> Option<QuotaReport> {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let entry = state.get(agent)?;
    if entry.retry_after <= now || entry.error.is_empty() {
        return None;
    }
    let mut report = QuotaReport::failed(agent, entry.kind.unwrap_or(ErrorKind::Unreachable), entry.error.clone());
    report.failures = entry.failures;
    report.failed_at = now;
    if let Some((windows, at)) = &entry.good {
        if !windows.is_empty() {
            report.windows = windows.clone();
            report.stale = true;
            report.updated_at = *at;
        }
    }
    Some(report)
}

/// A provider fetch, boxed so that the different providers can be waited for
/// together.
type Fetch = std::pin::Pin<Box<dyn std::future::Future<Output = QuotaReport> + Send>>;

/// Box a fetch so it can sit in a list next to the others.
fn boxed<F: std::future::Future<Output = QuotaReport> + Send + 'static>(f: F) -> Fetch {
    Box::pin(f)
}

/// Run one provider's fetch, bounded by a timeout, and settle the result.
///
/// The future is only awaited when the provider is not benched, so a provider
/// that is left alone after failing does not touch the network at all.
async fn run_provider<F>(agent: &str, fetch: F, timeout: Duration) -> QuotaReport
where
    F: std::future::Future<Output = QuotaReport> + Send,
{
    let now = now_secs();
    if let Some(report) = benched(agent, now) {
        return report;
    }
    let result = tokio::time::timeout(timeout, fetch).await;
    match result {
        Ok(report) => settle(report, now_secs()),
        Err(_) => settle(
            QuotaReport::failed(agent, ErrorKind::Timeout, "the provider did not answer in time"),
            now_secs(),
        ),
    }
}

// ------------------------------------------------------------------ the fetch

/// Fetch every live quota we can reach, using only credentials already on disk.
///
/// A provider that fails is reported with an error string and no windows; the
/// other providers are still returned.
pub async fn fetch_all(home: &Path) -> Vec<QuotaReport> {
    fetch_all_with(home, &Endpoints::default()).await
}

/// Fetch every live quota against a given set of endpoints.
pub async fn fetch_all_with(home: &Path, endpoints: &Endpoints) -> Vec<QuotaReport> {
    let client = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .user_agent("Cinder")
        .build()
    {
        Ok(c) => c,
        Err(e) => return vec![QuotaReport::failed("Cinder", ErrorKind::Unreachable, format!("no http client: {e}"))],
    };

    // Each account is fetched on its own, so a machine with two Claude
    // logins gets a card for each of them.
    let mut claude_creds = claude_credentials(home);
    let mut codex_creds = codex_credentials(home);
    // With a single account the plain name is used, so the alerts read the
    // way they always did.
    let many_claude = claude_creds.len() > 1;
    let many_codex = codex_creds.len() > 1;

    // A token that is about to expire is refreshed before it is used, so the
    // tracker does not report a problem that is only a clock.
    for cred in claude_creds.iter_mut() {
        if cred.refresh.is_some() && cred.expires_at > 0 && cred.expires_at <= now_secs() + 60 {
            if let Some(next) = refresh_claude(&client, endpoints, cred).await {
                *cred = next;
            }
        }
    }
    for cred in codex_creds.iter_mut() {
        if cred.refresh.is_some() && cred.expires_at > 0 && cred.expires_at <= now_secs() + 60 {
            if let Some(next) = refresh_codex(&client, endpoints, cred).await {
                *cred = next;
            }
        }
    }

    let mut fetches: Vec<Fetch> = Vec::new();
    for cred in claude_creds.iter() {
        fetches.push(boxed(run_provider(
            "Claude Code",
            claude_code(client.clone(), endpoints.clone(), cred.clone(), many_claude),
            PROVIDER_TIMEOUT,
        )));
    }
    for cred in codex_creds.iter() {
        fetches.push(boxed(run_provider(
            "Codex",
            codex(client.clone(), endpoints.clone(), cred.clone(), many_codex),
            PROVIDER_TIMEOUT,
        )));
    }
    fetches.push(boxed(run_provider(
        "GitHub Copilot",
        copilot(client.clone(), endpoints.clone(), home.to_path_buf()),
        PROVIDER_TIMEOUT,
    )));
    fetches.push(boxed(run_provider(
        "Gemini CLI",
        gemini_cli(client.clone(), endpoints.clone(), home.to_path_buf()),
        PROVIDER_TIMEOUT,
    )));
    fetches.push(boxed(run_provider(
        "OpenCode",
        opencode(client.clone(), endpoints.clone(), home.to_path_buf()),
        PROVIDER_TIMEOUT,
    )));

    let mut reports = Vec::new();
    for f in fetches {
        reports.push(f.await);
    }
    drop_spent(&mut reports, now_secs());
    reports
}

/// Drop the windows that have already reset.
///
/// A provider can still be holding a window whose reset time has passed, and
/// showing it would say a limit is in use when it is not.
pub fn drop_spent(reports: &mut [QuotaReport], now: i64) {
    for report in reports {
        report
            .windows
            .retain(|w| w.resets_at == 0 || w.resets_at > now);
    }
}

// --------------------------------------------------------------------- cache

/// The last quota snapshot, refreshed in the background so a scan never waits
/// on the network.
static SNAPSHOT: LazyLock<Mutex<Snapshot>> = LazyLock::new(|| Mutex::new(Snapshot::default()));

#[derive(Default)]
struct Snapshot {
    reports: Vec<QuotaReport>,
    checked_at: String,
    /// Unix seconds the snapshot was taken.
    checked_at_secs: i64,
    /// Whether the snapshot came off the disk rather than off the network.
    restored: bool,
}

/// The cached snapshot: the reports, when they were taken, and whether they
/// were restored from disk.
pub fn snapshot() -> (Vec<QuotaReport>, String, i64, bool) {
    let guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    (guard.reports.clone(), guard.checked_at.clone(), guard.checked_at_secs, guard.restored)
}

fn store(reports: Vec<QuotaReport>) {
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    guard.reports = reports;
    guard.checked_at = now_iso();
    guard.checked_at_secs = now_secs();
    guard.restored = false;
}

/// Publish the snapshot and return it, so the caller can push it to the UI.
pub fn publish(reports: Vec<QuotaReport>) -> (Vec<QuotaReport>, String, i64, bool) {
    store(reports);
    snapshot()
}

/// Where the snapshot is kept between runs.
pub fn cache_path(home: &Path) -> PathBuf {
    home.join(".cinder").join("quota_cache.json")
}

/// The snapshot as it is written to disk.
#[derive(Serialize, Deserialize)]
struct DiskSnapshot {
    version: u32,
    saved_at: i64,
    reports: Vec<QuotaReport>,
}

/// Write the snapshot to disk, so a launch has numbers before the first
/// refresh comes back.
pub fn save_cache(home: &Path, reports: &[QuotaReport]) {
    let disk = DiskSnapshot { version: CACHE_VERSION, saved_at: now_secs(), reports: reports.to_vec() };
    let Ok(value) = serde_json::to_value(&disk) else { return };
    let _ = write_json_atomic(&cache_path(home), &value);
}

/// Read the snapshot back from disk.
///
/// The numbers are marked as stale: they are what was true when the app was
/// last closed, not what is true now.
pub fn load_cache(home: &Path) -> Option<(Vec<QuotaReport>, i64)> {
    let v = read_json(&cache_path(home))?;
    if v.get("version").and_then(|x| x.as_u64()) != Some(CACHE_VERSION as u64) {
        return None;
    }
    let saved_at = v.get("saved_at").and_then(|x| x.as_i64()).unwrap_or(0);
    let mut reports: Vec<QuotaReport> = serde_json::from_value(v.get("reports")?.clone()).ok()?;
    for r in reports.iter_mut() {
        if !r.skipped && r.updated_at == 0 {
            r.updated_at = saved_at;
        }
        if !r.windows.is_empty() {
            r.stale = true;
        }
    }
    Some((reports, saved_at))
}

/// Put the snapshot that was on disk into the in-memory cache, if there is one.
///
/// Called once at start-up so the tray has something to show while the first
/// refresh is still in flight.
pub fn restore_cache(home: &Path) -> bool {
    let Some((mut reports, saved_at)) = load_cache(home) else { return false };
    // Windows that have reset while the app was closed are dropped.
    drop_spent(&mut reports, now_secs());
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    guard.reports = reports;
    guard.checked_at_secs = saved_at;
    guard.checked_at = iso_at(saved_at);
    guard.restored = true;
    true
}

/// The current time in unix seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The current time as an ISO-8601 UTC stamp.
pub fn now_iso() -> String {
    iso_at(now_secs())
}

/// A unix second as an ISO-8601 UTC stamp.
fn iso_at(secs: i64) -> String {
    let day = crate::unix_day(secs);
    let rem = secs.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{MockServer, Reply};
    use serde_json::json;

    #[test]
    fn claude_usage_reads_every_window() {
        let v = json!({
            "five_hour": { "utilization": 42.0, "resets_at": "2026-10-21T18:00:00Z" },
            "seven_day": { "utilization": 11.5, "resets_at": 1789000000 },
            "extra_usage": { "used_credits": 25.0, "monthly_limit": 50.0 }
        });
        let w = parse_claude_usage(&v, "max");
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].window, "5h");
        assert_eq!(w[0].used_percent, 42.0);
        assert_eq!(w[0].window_minutes, 300);
        assert_eq!(w[0].plan, "max");
        assert_eq!(w[1].window, "7d");
        assert_eq!(w[2].window, "monthly");
        assert_eq!(w[2].used_percent, 50.0);
    }

    #[test]
    fn codex_usage_reads_primary_and_secondary() {
        let v = json!({
            "rate_limits": {
                "primary": { "used_percent": 12.0, "window_minutes": 300, "resets_at": 1789000000 },
                "secondary": { "used_percent": 4.0, "window_minutes": 10080 }
            }
        });
        let w = parse_codex_usage(&v, "plus");
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].window, "5h");
        assert_eq!(w[0].used_percent, 12.0);
        assert_eq!(w[1].window, "7d");
        assert_eq!(w[1].used_percent, 4.0);
    }

    #[test]
    fn copilot_usage_turns_remaining_into_used() {
        let v = json!({
            "quota_snapshots": {
                "premium_interactions": { "entitlement": 300, "remaining": 120 }
            }
        });
        let w = parse_copilot_usage(&v);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].used_percent, 60.0);
    }

    #[test]
    fn gemini_usage_reads_quota_buckets() {
        let v = json!({
            "currentTier": { "id": "free-tier" },
            "quotaBuckets": [
                { "quotaId": "models/gemini-2.5-pro:freeTier", "quotaConsumed": 30, "quotaLimit": 1500 }
            ]
        });
        let w = parse_gemini_usage(&v);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].used_percent, 2.0);
        assert_eq!(w[0].plan, "free-tier");
    }

    #[test]
    fn an_unknown_payload_yields_no_windows() {
        assert!(parse_claude_usage(&json!({"nope": 1}), "").is_empty());
        assert!(parse_codex_usage(&json!({"nope": 1}), "").is_empty());
        assert!(parse_copilot_usage(&json!({"nope": 1})).is_empty());
        assert!(parse_gemini_usage(&json!({"nope": 1})).is_empty());
    }

    #[test]
    fn reset_times_accept_seconds_and_iso_strings() {
        assert_eq!(parse_resets_at(&json!(1789000000)), 1789000000);
        assert_eq!(parse_resets_at(&json!("2026-10-21")), days_from_civil(2026, 10, 21) * 86400);
    }

    #[test]
    fn an_iso_timestamp_keeps_its_time_of_day() {
        // The time of day matters: a window that resets at 18:00 is not one
        // that resets at midnight.
        let noon = parse_iso("2026-10-21T18:30:15Z");
        let midnight = days_from_civil(2026, 10, 21) * 86400;
        assert_eq!(noon - midnight, 18 * 3600 + 30 * 60 + 15);
        // An offset shifts the clock.
        assert_eq!(parse_iso("2026-10-21T18:30:15+02:00"), midnight + 16 * 3600 + 30 * 60 + 15);
    }

    #[test]
    fn the_codex_payload_shape_is_read_wherever_the_windows_sit() {
        // The shape the endpoint actually answers with: the windows live
        // under rate_limit, the reset is a moment, and the length is in
        // seconds.
        let v = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "limit_window_seconds": 18000,
                    "reset_at": 1792609864,
                    "used_percent": 100
                },
                "secondary_window": {
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 488274,
                    "used_percent": 78
                }
            }
        });
        let w = parse_codex_usage(&v, "plus");
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].window, "5h");
        assert_eq!(w[0].used_percent, 100.0);
        assert_eq!(w[0].window_minutes, 300);
        assert_eq!(w[0].resets_at, 1792609864);
        assert_eq!(w[1].window, "7d");
        assert_eq!(w[1].used_percent, 78.0);
        assert_eq!(w[1].window_minutes, 10080);
        // A countdown is turned into a moment.
        assert!(w[1].resets_at > now_secs());
    }

    #[test]
    fn a_window_that_has_already_reset_is_dropped() {
        let now = 1_000_000;
        let mut reports = vec![QuotaReport::ok("Codex", vec![
            LiveQuota { agent: "Codex".into(), window: "5h".into(), used_percent: 100.0, window_minutes: 300, resets_at: now - 60, ..Default::default() },
            LiveQuota { agent: "Codex".into(), window: "7d".into(), used_percent: 10.0, window_minutes: 10080, resets_at: now + 3600, ..Default::default() },
            LiveQuota { agent: "Codex".into(), window: "premium".into(), used_percent: 5.0, window_minutes: 0, resets_at: 0, ..Default::default() },
        ])];
        drop_spent(&mut reports, now);
        // The window that has reset is gone, the other two are not.
        let windows: Vec<&str> = reports[0].windows.iter().map(|w| w.window.as_str()).collect();
        assert_eq!(windows, vec!["7d", "premium"]);
    }

    #[test]
    fn percent_accepts_a_used_total_pair() {
        assert_eq!(percent(&json!({"used": 5.0, "total": 20.0})), Some(25.0));
        assert_eq!(percent(&json!(42.0)), Some(42.0));
        assert_eq!(percent(&json!({"used": 1.0, "total": 0.0})), None);
    }

    // ------------------------------------------------------- failure handling

    #[test]
    fn a_server_error_is_tried_again_and_then_succeeds() {
        let server = MockServer::start(vec![
            Reply::status(503, "busy"),
            Reply::ok(r#"{"five_hour":{"utilization":10}}"#),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(get_json(&server.client(), &server.url("/usage"), "t", &[]));
        assert!(v.is_ok(), "the second attempt should have worked");
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn a_refused_token_is_not_tried_again() {
        let server = MockServer::start(vec![Reply::status(401, "nope")]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(get_json(&server.client(), &server.url("/usage"), "t", &[]));
        assert_eq!(v.unwrap_err(), FetchError::Status(401));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn a_rate_limit_is_tried_again() {
        let server = MockServer::start(vec![
            Reply::status(429, "slow down"),
            Reply::ok(r#"{"ok":true}"#),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(get_json(&server.client(), &server.url("/usage"), "t", &[]));
        assert!(v.is_ok());
        assert_eq!(server.requests().len(), 2);
    }

    #[test]
    fn the_backoff_grows_and_then_stops_growing() {
        assert_eq!(backoff_seconds(1), 60);
        assert_eq!(backoff_seconds(2), 120);
        assert_eq!(backoff_seconds(3), 300);
        assert_eq!(backoff_seconds(4), 900);
        assert_eq!(backoff_seconds(40), 900);
    }

    #[test]
    fn a_failure_keeps_the_last_windows_and_marks_them_stale() {
        STATE.lock().unwrap().clear();
        let now = now_secs();
        let good = QuotaReport::ok(
            "Test Provider",
            vec![LiveQuota { agent: "Test Provider".into(), window: "5h".into(), used_percent: 30.0, ..Default::default() }],
        );
        let settled = settle(good, now);
        assert!(!settled.stale);
        assert_eq!(settled.windows.len(), 1);

        let failed = settle(QuotaReport::failed("Test Provider", ErrorKind::Unreachable, "no route to host"), now);
        assert!(failed.stale, "the last good windows should come back");
        assert_eq!(failed.windows.len(), 1);
        assert_eq!(failed.windows[0].used_percent, 30.0);
        assert_eq!(failed.failures, 1);
        assert_eq!(failed.error_kind, Some(ErrorKind::Unreachable));
        STATE.lock().unwrap().clear();
    }

    #[test]
    fn a_provider_that_keeps_failing_is_benched() {
        STATE.lock().unwrap().clear();
        let now = now_secs();
        settle(QuotaReport::failed("Bench Provider", ErrorKind::Timeout, "too slow"), now);
        // The next attempt inside the bench window does not touch the network.
        let report = benched("Bench Provider", now + 10).expect("the provider should be benched");
        assert_eq!(report.failures, 1);
        assert!(report.error.contains("too slow"));
        // After the bench it is asked again.
        assert!(benched("Bench Provider", now + 120).is_none());
        STATE.lock().unwrap().clear();
    }

    #[test]
    fn a_good_read_clears_the_bench() {
        STATE.lock().unwrap().clear();
        let now = now_secs();
        settle(QuotaReport::failed("Clear Provider", ErrorKind::Timeout, "too slow"), now);
        let ok = settle(
            QuotaReport::ok("Clear Provider", vec![LiveQuota { agent: "Clear Provider".into(), window: "5h".into(), ..Default::default() }]),
            now + 1,
        );
        assert!(!ok.stale);
        assert_eq!(ok.failures, 0);
        assert!(benched("Clear Provider", now + 2).is_none());
        STATE.lock().unwrap().clear();
    }

    #[test]
    fn the_snapshot_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("cinder-quota-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let reports = vec![QuotaReport::ok(
            "Claude Code",
            vec![LiveQuota { agent: "Claude Code".into(), window: "5h".into(), used_percent: 42.0, ..Default::default() }],
        )];
        save_cache(&dir, &reports);
        let (restored, saved_at) = load_cache(&dir).expect("the snapshot should come back");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].windows[0].used_percent, 42.0);
        assert!(restored[0].stale, "numbers read off the disk are not fresh");
        assert!(saved_at > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_from_another_version_is_not_read() {
        let dir = std::env::temp_dir().join(format!("cinder-quota-cache-v-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(cache_path(&dir).parent().unwrap().to_path_buf()).unwrap();
        let value = json!({ "version": 999, "saved_at": 1, "reports": [] });
        std::fs::write(cache_path(&dir), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load_cache(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_names_its_account_and_its_expiry() {
        // {"alg":"none"} . {"email":"me@example.com","exp":1700000000}
        let payload = "eyJhbGciOiJub25lIn0";
        let claims = "eyJlbWFpbCI6Im1lQGV4YW1wbGUuY29tIiwiZXhwIjoxNzAwMDAwMDAwfQ";
        let token = format!("{}.{}.sig", payload, claims);
        let (account, expires) = token_identity(&token);
        assert_eq!(account, "me@example.com");
        assert_eq!(expires, 1_700_000_000);
    }

    #[test]
    fn the_request_carries_the_token_the_agent_stored() {
        let server = MockServer::start(vec![Reply::ok(r#"{"ok":true}"#)]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(get_json(&server.client(), &server.url("/usage"), "secret-token", &[])).unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/usage");
        assert_eq!(requests[0].headers.get("authorization").map(|s| s.as_str()), Some("Bearer secret-token"));
    }

    #[test]
    fn a_provider_that_does_not_answer_in_time_is_given_up_on() {
        STATE.lock().unwrap().clear();
        let server = MockServer::start(vec![Reply::slow(r#"{"ok":true}"#, 400)]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let report = rt.block_on(run_provider(
            "Slow Provider",
            async {
                get_json(&server.client(), &server.url("/usage"), "t", &[])
                    .await
                    .map(|_| QuotaReport::ok("Slow Provider", Vec::new()))
                    .unwrap_or_else(|e| QuotaReport::failed("Slow Provider", e.kind(), e.message()))
            },
            Duration::from_millis(60),
        ));
        assert_eq!(report.error_kind, Some(ErrorKind::Timeout));
        assert!(report.error.contains("did not answer"));
        STATE.lock().unwrap().clear();
    }

    #[test]
    fn a_refresh_token_request_carries_the_grant() {
        let server = MockServer::start(vec![Reply::ok(r#"{"access_token":"fresh","expires_in":3600}"#)]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let got = rt.block_on(refresh_token(&server.client(), &server.url("/token"), "client-1", "refresh-1"));
        let (access, _next_refresh, expires) = got.expect("the token should have been refreshed");
        assert_eq!(access, "fresh");
        assert!(expires > now_secs());
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).expect("the body should be json");
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["client_id"], "client-1");
        assert_eq!(body["refresh_token"], "refresh-1");
    }

    #[test]
    fn a_provider_that_is_benched_is_not_asked_again() {
        STATE.lock().unwrap().clear();
        // Three failures in a row: the request is tried three times and then
        // the provider is benched.
        let server = MockServer::start(vec![
            Reply::status(503, "down"),
            Reply::status(503, "down"),
            Reply::status(503, "down"),
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let first = rt.block_on(run_provider(
            "Benched Provider",
            async {
                get_json(&server.client(), &server.url("/usage"), "t", &[])
                    .await
                    .map(|_| QuotaReport::ok("Benched Provider", Vec::new()))
                    .unwrap_or_else(|e| QuotaReport::failed("Benched Provider", e.kind(), e.message()))
            },
            Duration::from_secs(5),
        ));
        assert!(!first.error.is_empty(), "the provider should have failed");
        assert_eq!(server.requests().len(), 3, "the request should have been tried three times");
        // The second attempt inside the bench window does not reach the
        // network at all.
        let second = rt.block_on(run_provider(
            "Benched Provider",
            async {
                get_json(&server.client(), &server.url("/usage"), "t", &[])
                    .await
                    .map(|_| QuotaReport::ok("Benched Provider", Vec::new()))
                    .unwrap_or_else(|e| QuotaReport::failed("Benched Provider", e.kind(), e.message()))
            },
            Duration::from_secs(5),
        ));
        assert!(second.error.contains("HTTP"), "the benched provider should report the error it had");
        assert_eq!(server.requests().len(), 3, "a benched provider is not asked again");
        STATE.lock().unwrap().clear();
    }
}
