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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
    // "2026-10-21T18:00:00Z" -> unix seconds, without pulling in a date crate.
    let day = crate::day_of(s);
    if day.is_empty() {
        return 0;
    }
    let (y, m, d) = parse_day(&day);
    days_from_civil(y, m, d) * 86_400
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
            });
        }
    }
    out
}

/// Codex's usage payload: primary (5h) and secondary (weekly) windows.
fn parse_codex_usage(v: &serde_json::Value, plan: &str) -> Vec<LiveQuota> {
    let root = v.get("rate_limits").unwrap_or(v);
    let mut out = Vec::new();
    let windows: [(&str, &str, i64, &str); 2] = [
        ("primary", "5h", 300, "5-hour session window"),
        ("secondary", "7d", 7 * 24 * 60, "7-day window"),
    ];
    for (key, id, minutes, detail) in windows {
        let Some(w) = root.get(key) else { continue };
        let Some(pct) = w.get("used_percent").and_then(percent) else { continue };
        out.push(LiveQuota {
            agent: "Codex".into(),
            window: id.into(),
            used_percent: pct,
            window_minutes: w
                .get("window_minutes")
                .and_then(|x| x.as_i64())
                .unwrap_or(minutes),
            resets_at: w.get("resets_at").map(parse_resets_at).unwrap_or(0),
            plan: plan.into(),
            detail: detail.into(),
        });
    }
    out
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
    // The plan type lives on a second endpoint; a failure there is not fatal.
    let plan = get_json(
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
    .unwrap_or_default();
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
    vec![claude, codex_q, cop, gem, oc]
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
    fn percent_accepts_a_used_total_pair() {
        assert_eq!(percent(&json!({"used": 5.0, "total": 20.0})), Some(25.0));
        assert_eq!(percent(&json!(42.0)), Some(42.0));
        assert_eq!(percent(&json!({"used": 1.0, "total": 0.0})), None);
    }
}
