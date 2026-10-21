//! Live quota windows fetched from the providers' own APIs.
//!
//! Cinder reads local logs for usage, but the rate-limit windows a plan
//! enforces are only known to the provider. These endpoints answer with the
//! current utilisation of every window, using the OAuth token the agent
//! already stored on disk. Nothing leaves the machine except the request to
//! the provider itself, and a missing credential is simply skipped.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
}

impl QuotaReport {
    fn ok(agent: &str, windows: Vec<LiveQuota>) -> Self {
        Self { agent: agent.to_string(), windows, error: String::new(), skipped: false }
    }
    fn failed(agent: &str, error: impl Into<String>) -> Self {
        Self { agent: agent.to_string(), windows: Vec::new(), error: error.into(), skipped: false }
    }
    /// The agent is not signed in here, so there is nothing to report.
    fn absent(agent: &str) -> Self {
        Self { agent: agent.to_string(), windows: Vec::new(), error: String::new(), skipped: true }
    }
}

/// How long a single provider request may take before it is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// How often the background refresher asks the providers again.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

// ---------------------------------------------------------------- credentials

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
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

/// The Claude Code OAuth token, if the agent has been signed in.
fn claude_token(home: &Path) -> Option<String> {
    let mut candidates = vec![home.join(".claude").join(".credentials.json")];
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            candidates.push(PathBuf::from(dir).join(".credentials.json"));
        }
    }
    for path in candidates {
        let Some(v) = read_json(&path) else { continue };
        if let Some(t) = v
            .get("claudeAiOauth")
            .and_then(|o| o.get("accessToken"))
            .and_then(|x| x.as_str())
        {
            if !t.trim().is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// The ChatGPT access token Codex stores after signing in.
fn codex_token(home: &Path) -> Option<String> {
    let mut candidates = vec![home.join(".codex").join("auth.json")];
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.trim().is_empty() {
            candidates.push(PathBuf::from(dir).join("auth.json"));
        }
    }
    for path in candidates {
        let Some(v) = read_json(&path) else { continue };
        if let Some(t) = v
            .get("tokens")
            .and_then(|o| o.get("access_token"))
            .and_then(|x| x.as_str())
        {
            if !t.trim().is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
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

async fn get_json(client: &reqwest::Client, url: &str, token: &str, extra: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let mut req = client
        .get(url)
        .bearer_auth(token)
        .header("User-Agent", "Cinder")
        .header("Accept", "application/json");
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

async fn claude_code(home: &Path, client: &reqwest::Client) -> QuotaReport {
    let Some(token) = claude_token(home) else {
        return QuotaReport::absent("Claude Code");
    };
    let v = match get_json(
        client,
        "https://api.anthropic.com/api/oauth/usage",
        &token,
        &[("anthropic-beta", "oauth-2025-04-20")],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("Claude Code", e),
    };
    let plan = v
        .get("plan")
        .or_else(|| v.get("subscription_plan"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let windows = parse_claude_usage(&v, &plan);
    if windows.is_empty() {
        return QuotaReport::failed("Claude Code", "unexpected usage payload");
    }
    QuotaReport::ok("Claude Code", windows)
}

async fn codex(home: &Path, client: &reqwest::Client) -> QuotaReport {
    let Some(token) = codex_token(home) else {
        return QuotaReport::absent("Codex");
    };
    let usage = get_json(
        client,
        "https://chatgpt.com/backend-api/wham/usage",
        &token,
        &[("OpenAI-Beta", "codex-1")],
    )
    .await;
    let Ok(usage) = usage else {
        return QuotaReport::failed("Codex", usage.unwrap_err());
    };
    // The plan type is in the usage payload itself; the second endpoint is
    // only a fallback.
    let plan = match usage.get("plan_type").and_then(|x| x.as_str()) {
        Some(p) => p.to_string(),
        None => get_json(
            client,
            "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits",
            &token,
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
    let windows = parse_codex_usage(&usage, &plan);
    if windows.is_empty() {
        return QuotaReport::failed("Codex", "unexpected usage payload");
    }
    QuotaReport::ok("Codex", windows)
}

async fn copilot(home: &Path, client: &reqwest::Client) -> QuotaReport {
    let Some(token) = copilot_token(home) else {
        return QuotaReport::absent("GitHub Copilot");
    };
    let v = match get_json(
        client,
        "https://api.github.com/copilot_internal/user",
        &token,
        &[
            ("Accept", "application/vnd.github+json"),
            ("Copilot-Integration-Id", "vscode-chat"),
        ],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("GitHub Copilot", e),
    };
    let windows = parse_copilot_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("GitHub Copilot", "unexpected usage payload");
    }
    QuotaReport::ok("GitHub Copilot", windows)
}

async fn gemini_cli(home: &Path, client: &reqwest::Client) -> QuotaReport {
    let Some(token) = gemini_token(home) else {
        return QuotaReport::absent("Gemini CLI");
    };
    let v = match get_json(
        client,
        "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
        &token,
        &[("Content-Type", "application/json")],
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("Gemini CLI", e),
    };
    let windows = parse_gemini_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("Gemini CLI", "unexpected usage payload");
    }
    QuotaReport::ok("Gemini CLI", windows)
}

async fn opencode(home: &Path, client: &reqwest::Client) -> QuotaReport {
    let Some(token) = opencode_token(home) else {
        return QuotaReport::absent("OpenCode");
    };
    let v = match get_json(client, "https://opencode.ai/api/usage", &token, &[]).await {
        Ok(v) => v,
        Err(e) => return QuotaReport::failed("OpenCode", e),
    };
    let windows = parse_opencode_usage(&v);
    if windows.is_empty() {
        return QuotaReport::failed("OpenCode", "unexpected usage payload");
    }
    QuotaReport::ok("OpenCode", windows)
}

/// The current time in unix seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fetch every live quota we can reach, using only credentials already on disk.
///
/// A provider that fails is reported with an error string and no windows; the
/// other providers are still returned.
pub async fn fetch_all(home: &Path) -> Vec<QuotaReport> {
    let client = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Cinder")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return vec![QuotaReport::failed("Cinder", format!("no http client: {e}"))]
        }
    };
    let (claude, codex_q, cop, gem, oc) = tokio::join!(
        claude_code(home, &client),
        codex(home, &client),
        copilot(home, &client),
        gemini_cli(home, &client),
        opencode(home, &client),
    );
    let mut reports = vec![claude, codex_q, cop, gem, oc];
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
static SNAPSHOT: std::sync::LazyLock<std::sync::Mutex<Snapshot>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Snapshot::default()));

#[derive(Default)]
struct Snapshot {
    reports: Vec<QuotaReport>,
    checked_at: String,
}

/// The cached snapshot: the reports and when they were taken.
pub fn snapshot() -> (Vec<QuotaReport>, String) {
    let guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    (guard.reports.clone(), guard.checked_at.clone())
}

fn store(reports: Vec<QuotaReport>) {
    let mut guard = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    guard.reports = reports;
    guard.checked_at = now_iso();
}

/// Publish the snapshot and return it, so the caller can push it to the UI.
pub fn publish(reports: Vec<QuotaReport>) -> (Vec<QuotaReport>, String) {
    store(reports);
    snapshot()
}

/// The current time as an ISO-8601 UTC stamp.
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
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
}
