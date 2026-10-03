// Cinder — coding agent usage statistics collector
//
// Parses the local session stores of the coding agents installed on this machine
// and emits per-day, per-agent, per-model rows. The UI aggregates by time range.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pricing;

use pricing::Pricing;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Default, Clone)]
struct Row {
    day: String,
    agent: String,
    model: String,
    requests: u64,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    tools: u64,
    cost: f64,
    cost_known: bool,
    savings: f64,
}

impl Row {
    fn tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

#[derive(Serialize, Clone)]
struct SessionRow {
    day: String,
    agent: String,
    sessions: u64,
}

#[derive(Serialize, Clone)]
struct LimitRow {
    agent: String,
    window: String,
    used_percent: f64,
    window_minutes: i64,
    resets_at: i64,
    plan: String,
}

#[derive(Serialize)]
struct Source {
    agent: String,
    path: String,
    found: bool,
    files: u64,
}

#[derive(Serialize)]
struct Stats {
    rows: Vec<Row>,
    sessions: Vec<SessionRow>,
    limits: Vec<LimitRow>,
    sources: Vec<Source>,
    pricing: PricingInfo,
    /// tokens re-priced because the reporting agent did not expose cache hits
    adjusted_tokens: u64,
    unpriced: Vec<String>,
    /// providers covered by a subscription/plan instead of per-token billing
    plan_routes: Vec<String>,
}

#[derive(Serialize)]
struct PricingInfo {
    source: String,
    embedded: usize,
    local: usize,
}

type Rows = HashMap<(String, String, String), Row>;
type Sess = HashMap<(String, String), u64>;

/// Observed cache-hit ratio per model, learned from agents that report cache usage
/// truthfully. Routes behind a proxy (Codex -> opencode-go) often report
/// cached_input_tokens = 0 even though the upstream caches ~95% of the prompt,
/// which makes a naive cost estimate ~20x too high.
#[derive(Default)]
struct CacheRatios {
    cache: HashMap<String, u64>,
    total: HashMap<String, u64>,
    adjusted: u64,
}

impl CacheRatios {
    fn learn(&mut self, rows: &Rows) {
        for r in rows.values() {
            if r.cache_read == 0 {
                continue;
            }
            let key = model_tail(&r.model);
            *self.cache.entry(key.clone()).or_default() += r.cache_read;
            *self.total.entry(key).or_default() += r.input + r.cache_read + r.cache_write;
        }
    }

    fn observed(&self, model: &str) -> Option<f64> {
        let key = model_tail(model);
        let cache = *self.cache.get(&key)? as f64;
        let total = *self.total.get(&key)? as f64;
        if total < 100_000.0 {
            return None;
        }
        let r = cache / total;
        if r >= 0.5 {
            Some(r)
        } else {
            None
        }
    }
}

fn model_tail(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_lowercase()
}

/// Provider prefix of a model string ("opencode-go/deepseek-v4-flash" -> "opencode-go").
fn model_provider(model: &str) -> Option<&str> {
    model.split_once('/').map(|(p, _)| p).filter(|p| !p.is_empty() && !p.contains(' '))
}

fn add(rows: &mut Rows, sess: &mut Sess, agent: &str, model: &str, day: &str, r: Row) {
    let key = (day.to_string(), agent.to_string(), model.to_string());
    let e = rows.entry(key).or_insert_with(|| Row {
        day: day.to_string(),
        agent: agent.to_string(),
        model: model.to_string(),
        ..Default::default()
    });
    e.requests += r.requests;
    e.input += r.input;
    e.cache_read += r.cache_read;
    e.cache_write += r.cache_write;
    e.output += r.output;
    e.reasoning += r.reasoning;
    e.tools += r.tools;
    e.cost += r.cost;
    e.cost_known = e.cost_known || r.cost_known;
    e.savings += r.savings;
    let _ = sess;
}

fn session(sess: &mut Sess, agent: &str, day: &str) {
    *sess.entry((day.to_string(), agent.to_string())).or_insert(0) += 1;
}

fn day_of(ts: &str) -> String {
    ts.chars().take(10).filter(|c| *c != '\u{0}').collect()
}

fn list_files(root: &Path, ext: &str, name: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, ext: &str, name: Option<&str>, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, ext, name, out);
                } else {
                    let ext_ok = p.extension().map(|x| x == ext).unwrap_or(false);
                    let name_ok = match name {
                        Some(n) => p.file_name().map(|f| f == n).unwrap_or(false),
                        None => true,
                    };
                    if ext_ok && name_ok {
                        out.push(p);
                    }
                }
            }
        }
    }
    if root.exists() {
        walk(root, ext, name, &mut out);
    }
    out
}

fn read_lines(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

// ---------------- Pi ----------------
fn parse_pi(home: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess, plan_routes: &mut HashMap<String, u64>) -> u64 {
    let root = home.join(".pi").join("agent").join("sessions");
    let files = list_files(&root, "jsonl", None);
    for file in &files {
        let Some(text) = read_lines(file) else { continue };
        let mut day = String::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
            if v.get("type").and_then(|t| t.as_str()) == Some("session") && day.is_empty() {
                day = day_of(ts);
            }
            if v.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            let Some(msg) = v.get("message") else { continue };
            if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            let u = msg.get("usage").cloned().unwrap_or(serde_json::Value::Null);
            let input = u.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
            let output = u.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
            let cache_read = u.get("cacheRead").and_then(|x| x.as_u64()).unwrap_or(0);
            let cache_write = u.get("cacheWrite").and_then(|x| x.as_u64()).unwrap_or(0);
            let reasoning = u.get("reasoning").and_then(|x| x.as_u64()).unwrap_or(0);
            if input + output + cache_read + cache_write == 0 {
                continue;
            }
            let mut tools = 0u64;
            if let Some(content) = msg.get("content").and_then(|x| x.as_array()) {
                for item in content {
                    if item.get("type").and_then(|t| t.as_str()) == Some("toolCall") {
                        tools += 1;
                    }
                }
            }
            let model = pricing::normalize_model(msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"));
            let provider = msg.get("provider").and_then(|p| p.as_str()).map(|s| s.to_lowercase());
            let mut r = Row {
                day: day_of(ts),
                agent: "Pi".into(),
                model: model.clone(),
                requests: 1,
                input,
                cache_read,
                cache_write,
                output,
                reasoning,
                tools,
                ..Default::default()
            };
            // pi accounts for its own routes. A reported zero is meaningful: it means the
            // route is a plan/subscription or has no per-token billing (e.g. zed-ai),
            // so it must not be re-priced with a same-named model from another provider.
            let reported = u.get("cost").and_then(|c| c.get("total")).and_then(|x| x.as_f64());
            let priced = pricing.lookup(provider.as_deref(), &model);
            match reported {
                Some(c) if c > 0.0 => {
                    r.cost = c;
                    r.cost_known = true;
                }
                Some(_) if provider.is_some() && priced.is_none() => {
                    // plan route: covered by the subscription, not billed per token
                    r.cost = 0.0;
                    r.cost_known = true;
                }
                _ => {
                    if let Some(p) = priced {
                        r.cost = p.cost(input, cache_read, cache_write, output);
                        r.cost_known = true;
                    }
                }
            }
            if let Some(p) = priced {
                r.savings = p.savings(cache_read, cache_write);
            }
            if provider.is_some() && reported == Some(0.0) && priced.is_none() {
                if let Some(p) = provider.as_deref() {
                    *plan_routes.entry(p.to_string()).or_insert(0) += input + cache_read + cache_write + output;
                }
            }
            if r.day.is_empty() {
                r.day = day.clone();
            }
            add(rows, sess, "Pi", &model, &r.day.clone(), r);
        }
        if !day.is_empty() {
            session(sess, "Pi", &day);
        }
    }
    files.len() as u64
}

// ---------------- Codex ----------------
fn parse_codex(home: &Path, pricing: &Pricing, ratios: &mut CacheRatios, rows: &mut Rows, sess: &mut Sess, limits: &mut Vec<LimitRow>) -> u64 {
    let root = home.join(".codex").join("sessions");
    let files = list_files(&root, "jsonl", None);
    for file in &files {
        let Some(text) = read_lines(file) else { continue };
        let mut model = String::from("unknown");
        let mut day = String::new();
        let mut latest: Option<(String, f64, i64, i64)> = None;
        let mut plan = String::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let p = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
            if day.is_empty() && ty == "session_meta" {
                day = day_of(ts);
            }
            match ty {
                "turn_context" => {
                    if let Some(m) = p.get("model").and_then(|m| m.as_str()) {
                        model = pricing::normalize_model(m);
                    }
                }
                "response_item" => {
                    let pt = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if pt == "function_call" || pt == "custom_tool_call" {
                        let mut r = Row {
                            day: day_of(ts),
                            agent: "Codex".into(),
                            model: model.clone(),
                            tools: 1,
                            ..Default::default()
                        };
                        if r.day.is_empty() { r.day = day.clone(); }
                        add(rows, sess, "Codex", &model, &r.day.clone(), r);
                    }
                }
                "event_msg" => {
                    if p.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                        continue;
                    }
                    if let Some(ptype) = p.get("rate_limits").and_then(|rl| rl.get("plan_type")).and_then(|x| x.as_str()) {
                        plan = ptype.to_string();
                    }
                    if let Some(rl) = p.get("rate_limits") {
                        for key in ["primary", "secondary"] {
                            if let Some(w) = rl.get(key) {
                                if let Some(pct) = w.get("used_percent").and_then(|x| x.as_f64()) {
                                    latest = Some((
                                        key.to_string(),
                                        pct,
                                        w.get("window_minutes").and_then(|x| x.as_i64()).unwrap_or(0),
                                        w.get("resets_at").and_then(|x| x.as_i64()).unwrap_or(0),
                                    ));
                                }
                            }
                        }
                    }
                    let Some(info) = p.get("info") else { continue };
                    let Some(last) = info.get("last_token_usage") else { continue };
                    let input = last.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let cached = last.get("cached_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let output = last.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let reasoning = last.get("reasoning_output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    if input + output == 0 {
                        continue;
                    }
                    let mut uncached = input.saturating_sub(cached);
                    let total_in = input;
                    // The proxy in front of Codex does not surface cache-hit tokens.
                    // Re-derive the cache ratio from clients that do report it.
                    let reported_ratio = if total_in > 0 { cached as f64 / total_in as f64 } else { 0.0 };
                    if let Some(observed) = ratios.observed(&model) {
                        if total_in >= 100_000 && reported_ratio < observed * 0.5 {
                            let new_cached = (total_in as f64 * observed).round() as u64;
                            uncached = total_in.saturating_sub(new_cached);
                            ratios.adjusted += total_in;
                        }
                    }
                    let cached = total_in.saturating_sub(uncached);
                    let mut r = Row {
                        day: day_of(ts),
                        agent: "Codex".into(),
                        model: model.clone(),
                        requests: 1,
                        input: uncached,
                        cache_read: cached,
                        cache_write: 0,
                        output,
                        reasoning,
                        ..Default::default()
                    };
                    if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
                        r.cost = pr.cost(uncached, cached, 0, output);
                        r.cost_known = true;
                        r.savings = pr.savings(cached, 0);
                    }
                    if r.day.is_empty() { r.day = day.clone(); }
                    add(rows, sess, "Codex", &model, &r.day.clone(), r);
                }
                _ => {}
            }
        }
        if let Some((window, pct, wm, ra)) = latest {
            let entry = limits
                .iter_mut()
                .find(|l: &&mut LimitRow| l.agent == "Codex" && l.window == window && l.plan == plan);
            match entry {
                Some(l) => {
                    l.used_percent = l.used_percent.max(pct);
                    l.window_minutes = wm;
                    l.resets_at = ra;
                }
                None => limits.push(LimitRow {
                    agent: "Codex".into(),
                    window,
                    used_percent: pct,
                    window_minutes: wm,
                    resets_at: ra,
                    plan: plan.clone(),
                }),
            }
        }
        if !day.is_empty() {
            session(sess, "Codex", &day);
        }
    }
    files.len() as u64
}

// ---------------- Claude Code ----------------
fn parse_claude(home: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) -> u64 {
    let projects = home.join(".claude").join("projects");
    if !projects.exists() {
        return 0;
    }
    let files = list_files(&projects, "jsonl", None);
    for file in &files {
        let Some(text) = read_lines(file) else { continue };
        let mut day = String::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("");
            if day.is_empty() && !day_of(ts).is_empty() {
                day = day_of(ts);
            }
            if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(msg) = v.get("message") else { continue };
            let Some(u) = msg.get("usage") else { continue };
            let input = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let output = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let cache_read = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let cache_write = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            if input + output + cache_read + cache_write == 0 {
                continue;
            }
            let mut tools = 0u64;
            if let Some(content) = msg.get("content").and_then(|x| x.as_array()) {
                for item in content {
                    if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        tools += 1;
                    }
                }
            }
            let model = pricing::normalize_model(msg.get("model").and_then(|m| m.as_str()).unwrap_or("unknown"));
            let mut r = Row {
                day: day_of(ts),
                agent: "Claude Code".into(),
                model: model.clone(),
                requests: 1,
                input,
                cache_read,
                cache_write,
                output,
                reasoning: 0,
                tools,
                ..Default::default()
            };
            if let Some(pr) = pricing.lookup(model_provider(&model), &model) {
                r.cost = pr.cost(input, cache_read, cache_write, output);
                r.cost_known = true;
                r.savings = pr.savings(cache_read, cache_write);
            }
            if r.day.is_empty() { r.day = day.clone(); }
            add(rows, sess, "Claude Code", &model, &r.day.clone(), r);
        }
        if !day.is_empty() {
            session(sess, "Claude Code", &day);
        }
    }
    files.len() as u64
}

// ---------------- OpenCode ----------------
fn parse_opencode(home: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) -> u64 {
    let dir = home.join(".local").join("share").join("opencode");
    let db = dir.join("opencode.db");
    if !db.exists() {
        return 0;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return 0;
    };
    let table = ["session_v2", "session"].iter().find(|t| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {}", t), [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)
            .unwrap_or(false)
    });
    let Some(table) = table else { return 0 };
    let mut count = 0u64;
    let sql = format!(
        "SELECT model, cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write, tokens_reasoning, time_created, time_updated FROM {}",
        table
    );
    if let Ok(mut stmt) = conn.prepare(&sql) {
        let rows_iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        });
        if let Ok(it) = rows_iter {
            for row in it.flatten() {
                let (model, cost, ti, to, tcr, tcw, tr, tcreated, tupdated) = row;
                count += 1;
                let input = ti.unwrap_or(0).max(0) as u64;
                let output = to.unwrap_or(0).max(0) as u64;
                let cache_read = tcr.unwrap_or(0).max(0) as u64;
                let cache_write = tcw.unwrap_or(0).max(0) as u64;
                let reasoning = tr.unwrap_or(0).max(0) as u64;
                let model = pricing::normalize_model(&model.unwrap_or_else(|| "unknown".into()));
                let ts = tupdated.or(tcreated).unwrap_or(0);
                let secs = if ts > 1_000_000_000_000 { ts / 1000 } else { ts };
                let day = unix_day(secs);
                let mut r = Row {
                    day: day.clone(),
                    agent: "OpenCode".into(),
                    model: model.clone(),
                    requests: 1,
                    input,
                    cache_read,
                    cache_write,
                    output,
                    reasoning,
                    ..Default::default()
                };
                match cost {
                    Some(c) if c > 0.0 => {
                        r.cost = c;
                        r.cost_known = true;
                    }
                    Some(_) => {
                        r.cost = 0.0;
                        r.cost_known = true;
                    }
                    _ => {
                        if let Some(p) = pricing.lookup(model_provider(&model), &model) {
                            r.cost = p.cost(input, cache_read, cache_write, output);
                            r.cost_known = true;
                            r.savings = p.savings(cache_read, cache_write);
                        }
                    }
                }
                if r.savings == 0.0 {
                    if let Some(p) = pricing.lookup(model_provider(&model), &model) {
                        r.savings = p.savings(cache_read, cache_write);
                    }
                }
                add(rows, sess, "OpenCode", &model, &day, r);
                session(sess, "OpenCode", &day);
            }
        }
    }
    // tool calls, attributed to the owning session's model and day
    let tools_sql = format!(
        "SELECT s.model, p.time_created, COUNT(*) FROM part p \
         JOIN message m ON p.message_id = m.id JOIN {} s ON m.session_id = s.id \
         WHERE p.data LIKE '%\"type\":\"tool\"%' GROUP BY s.model, p.time_created",
        table
    );
    if let Ok(mut stmt) = conn.prepare(&tools_sql) {
        if let Ok(it) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        }) {
            for (model, ts, n) in it.flatten() {
                let secs = ts.map(|t| if t > 1_000_000_000_000 { t / 1000 } else { t }).unwrap_or(0);
                let day = unix_day(secs);
                let model = pricing::normalize_model(&model.unwrap_or_else(|| "unknown".into()));
                add(
                    rows,
                    sess,
                    "OpenCode",
                    &model,
                    &day,
                    Row { tools: n.unwrap_or(0).max(0) as u64, ..Default::default() },
                );
            }
        }
    }
    count
}

// ---------------- Antigravity ----------------
fn parse_antigravity(home: &Path, rows: &mut Rows, sess: &mut Sess) -> u64 {
    let mut dbs = 0u64;
    for variant in ["antigravity", "antigravity-cli"] {
        let conv = home.join(".gemini").join(variant).join("conversations");
        dbs += list_files(&conv, "db", None).len() as u64;
        let brain = home.join(".gemini").join(variant).join("brain");
        let transcripts = list_files(&brain, "jsonl", Some("transcript.jsonl"));
        for file in &transcripts {
            let Some(text) = read_lines(file) else { continue };
            let mtime_day = fs::metadata(file)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| unix_day(d.as_secs() as i64))
                .unwrap_or_default();
            session(sess, "Antigravity", &mtime_day);
            let mut model = String::from("unknown");
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                if let Some(content) = v.get("content").and_then(|x| x.as_str()) {
                    if let Some(idx) = content.rfind("Model Selection` from") {
                        let rest = &content[idx..];
                        if let Some(to) = rest.find(" to ") {
                            let tail = &rest[to + 4..];
                            if let Some(end) = tail.find('.') {
                                let m = tail[..end].trim();
                                if !m.is_empty() && m != "None" {
                                    model = m.to_string();
                                }
                            }
                        }
                    }
                }
                if v.get("type").and_then(|t| t.as_str()) == Some("PLANNER_RESPONSE") {
                    let tools = v.get("tool_calls").and_then(|t| t.as_array()).map(|a| a.len() as u64).unwrap_or(0);
                    let ts = v.get("created_at").and_then(|t| t.as_str()).unwrap_or("");
                    let r = Row {
                        day: day_of(ts),
                        agent: "Antigravity".into(),
                        model: model.clone(),
                        requests: 1,
                        tools,
                        ..Default::default()
                    };
                    add(rows, sess, "Antigravity", &model, &r.day.clone(), r);
                }
            }
        }
        // conversation dbs without transcripts still count as sessions (dated by file mtime)
        if transcripts.is_empty() {
            for db in list_files(&conv, "db", None) {
                let day = fs::metadata(&db)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| unix_day(d.as_secs() as i64))
                    .unwrap_or_default();
                session(sess, "Antigravity", &day);
            }
        }
    }
    dbs
}

fn unix_day(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs / 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[tauri::command]
fn get_stats() -> Stats {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let pricing = Pricing::load(&home);
    let mut rows = Rows::new();
    let mut sess = Sess::new();
    let mut limits = Vec::new();
    let mut ratios = CacheRatios::default();
    let mut plan_routes: HashMap<String, u64> = HashMap::new();

    // parse the agents that report cache usage truthfully first, so Codex can borrow their ratios
    let pi_files = parse_pi(&home, &pricing, &mut rows, &mut sess, &mut plan_routes);
    let claude_files = parse_claude(&home, &pricing, &mut rows, &mut sess);
    let opencode_n = parse_opencode(&home, &pricing, &mut rows, &mut sess);
    let agy_dbs = parse_antigravity(&home, &mut rows, &mut sess);
    ratios.learn(&rows);

    let codex_files = parse_codex(&home, &pricing, &mut ratios, &mut rows, &mut sess, &mut limits);

    let sources = vec![
        Source { agent: "Pi".into(), path: "~/.pi/agent/sessions".into(), found: pi_files > 0, files: pi_files },
        Source { agent: "Codex".into(), path: "~/.codex/sessions".into(), found: codex_files > 0, files: codex_files },
        Source { agent: "Claude Code".into(), path: "~/.claude/projects".into(), found: claude_files > 0, files: claude_files },
        Source { agent: "OpenCode".into(), path: "~/.local/share/opencode/opencode.db".into(), found: opencode_n > 0, files: opencode_n },
        Source { agent: "Antigravity".into(), path: "~/.gemini/antigravity*/conversations".into(), found: agy_dbs > 0, files: agy_dbs },
    ];

    let mut rows: Vec<Row> = rows.into_values().collect();
    rows.sort_by(|a, b| a.day.cmp(&b.day).then(a.agent.cmp(&b.agent)));
    let mut sessions: Vec<SessionRow> = sess
        .into_iter()
        .map(|((day, agent), n)| SessionRow { day, agent, sessions: n })
        .collect();
    sessions.sort_by(|a, b| a.day.cmp(&b.day));

    let mut unpriced: Vec<(String, u64)> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for r in rows.iter().filter(|r| r.tokens() > 0) {
        if r.cost_known && seen.insert(r.model.clone(), ()).is_none() {
            continue;
        }
        if !r.cost_known && !r.model.is_empty() && seen.insert(r.model.clone(), ()).is_none() {
            unpriced.push((r.model.clone(), r.tokens()));
        }
    }
    unpriced.sort_by(|a, b| b.1.cmp(&a.1));

    Stats {
        rows,
        sessions,
        limits,
        sources,
        adjusted_tokens: ratios.adjusted,
        unpriced: unpriced.into_iter().take(12).map(|(m, _)| m).collect(),
        plan_routes: {
            let mut v: Vec<(String, u64)> = plan_routes.into_iter().collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.into_iter().map(|(p, _)| p).collect()
        },
        pricing: PricingInfo {
            source: pricing.source.clone(),
            embedded: pricing.embedded_count,
            local: pricing.local_count,
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_stats])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run();
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    #[test]
    fn dump_stats() {
        let s = super::get_stats();
        let rows: u64 = s.rows.iter().map(|r| r.requests + r.tools).sum();
        eprintln!("rows={} events={} sessions={} limits={} pricing={}", s.rows.len(), rows, s.sessions.len(), s.limits.len(), s.pricing.embedded + s.pricing.local);
        let json = serde_json::to_string(&s).unwrap();
        std::fs::write(std::env::temp_dir().join("cinder-stats.json"), &json).unwrap();
        // summary per agent
        let mut per: HashMap<String, (u64, u64, f64, u64)> = HashMap::new();
        for r in &s.rows {
            let e = per.entry(r.agent.clone()).or_default();
            e.0 += r.requests;
            e.1 += r.input + r.output + r.cache_read + r.cache_write;
            e.2 += r.cost;
            e.3 += r.tools;
        }
        for (a, v) in per {
            eprintln!("{:14} req={:7} tokens={:14} cost=${:9.2} tools={}", a, v.0, v.1, v.2, v.3);
        }
        let mut priced = 0;
        let mut unpriced: HashMap<String, u64> = HashMap::new();
        for r in &s.rows {
            if r.cost_known { priced += r.tokens() } else { *unpriced.entry(r.model.clone()).or_default() += r.tokens(); }
        }
        eprintln!("priced tokens={} unpriced models={}", priced, unpriced.len());
        let mut u: Vec<_> = unpriced.into_iter().collect();
        u.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
        for (m, t) in u.into_iter().take(12) {
            eprintln!("  unpriced {:50} {}", m, t);
        }
    }
}
