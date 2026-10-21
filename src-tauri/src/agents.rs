// Parsers for the coding agents whose local logs Cinder does not read yet:
// Cursor, GitHub Copilot, Gemini CLI and Cline.
//
// None of them share a log format, so every parser here walks its own
// directories, pulls out the token counts it recognises and leaves everything
// it cannot see at zero. Nothing is guessed: a field that is not in the log
// stays zero and the row keeps an unknown cost.
//
// The cache pattern is the same as in main.rs: the file stamp decides whether a
// file is replayed from the cache or parsed again.

use crate::cache::{file_stamp, CachedFile, ScanCache};
use crate::prefilter::Prefilter;
use crate::pricing::{normalize_model, Pricing};
use crate::project::label_for_cwd;
use crate::{add, day_of, model_provider, session, unix_day, Billing, Row, Rows, Sess};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const CURSOR: &str = "Cursor";
const COPILOT: &str = "GitHub Copilot";
const GEMINI: &str = "Gemini CLI";
const CLINE: &str = "Cline";

// ---------------- token blocks ----------------

/// The token counts of one request.
#[derive(Default, Clone, Copy, Debug)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

/// The keys the agents use for the same token count. Every agent names its
/// counters differently, so each list holds the spellings seen in the wild.
const INPUT_KEYS: &[&str] = &[
    "input",
    "input_tokens",
    "inputTokens",
    "prompt_tokens",
    "promptTokens",
    "tokensIn",
    "input_token_count",
    "inputTokenCount",
    "promptTokenCount",
];
const OUTPUT_KEYS: &[&str] = &[
    "output",
    "output_tokens",
    "outputTokens",
    "completion_tokens",
    "completionTokens",
    "tokensOut",
    "output_token_count",
    "outputTokenCount",
    "candidatesTokenCount",
];
const CACHE_READ_KEYS: &[&str] = &[
    "cached",
    "cached_tokens",
    "cachedTokens",
    "cache_read",
    "cacheRead",
    "cache_read_input_tokens",
    "cache_reads",
    "cacheReads",
    "cacheReadsTokens",
    "cached_input_tokens",
    "cachedInputTokens",
    "cachedContentTokenCount",
];
const CACHE_WRITE_KEYS: &[&str] = &[
    "cache_write",
    "cacheWrite",
    "cache_writes",
    "cacheWritesTokens",
    "cache_creation_input_tokens",
    "cacheCreationTokenCount",
];
const REASONING_KEYS: &[&str] = &[
    "reasoning",
    "reasoning_tokens",
    "reasoningTokens",
    "thoughts",
    "thoughtsTokenCount",
    "thoughts_token_count",
];
const TOTAL_KEYS: &[&str] = &["total", "totalTokens", "total_tokens"];

/// The keys that hold a usage block in a larger object.
const USAGE_CONTAINERS: &[&str] = &[
    "usage",
    "tokenUsage",
    "token_usage",
    "tokens",
    "stats",
    "tokenCounts",
    "token_count",
    "totalUsage",
];

fn pick(obj: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| obj.get(*k).and_then(|v| v.as_u64()))
}

/// A usage block: an object with at least one of the token counters in it.
fn usage_block(v: &Value) -> Option<Usage> {
    let obj = v.as_object()?;
    let mut u = Usage::default();
    let mut found = false;
    if let Some(n) = pick(obj, INPUT_KEYS) {
        u.input = n;
        found = true;
    }
    if let Some(n) = pick(obj, OUTPUT_KEYS) {
        u.output = n;
        found = true;
    }
    if let Some(n) = pick(obj, CACHE_READ_KEYS) {
        u.cache_read = n;
        found = true;
    }
    if let Some(n) = pick(obj, CACHE_WRITE_KEYS) {
        u.cache_write = n;
        found = true;
    }
    if let Some(n) = pick(obj, REASONING_KEYS) {
        u.reasoning = n;
        found = true;
    }
    if !found {
        return None;
    }
    // openai hides the cached and reasoning counts one level deeper
    if u.cache_read == 0 {
        if let Some(n) = obj
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|x| x.as_u64())
        {
            u.cache_read = n;
        }
    }
    if u.reasoning == 0 {
        if let Some(n) = obj
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|x| x.as_u64())
        {
            u.reasoning = n;
        }
    }
    Some(u)
}

/// A usage block that carries nothing but a total. The input/output split is
/// not in the log, so the total is counted as input: the token count stays
/// right and the price is the lower bound of the real one.
fn usage_total_only(v: &Value) -> Option<Usage> {
    let obj = v.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let total = pick(obj, TOTAL_KEYS)?;
    Some(Usage {
        input: total,
        ..Default::default()
    })
}

/// The usage block of an object, looking through the known container keys.
fn usage_from_block(v: &Value) -> Option<Usage> {
    if let Some(u) = usage_block(v) {
        return Some(u);
    }
    for key in USAGE_CONTAINERS {
        if let Some(inner) = v.get(key) {
            if let Some(u) = usage_block(inner).or_else(|| usage_total_only(inner)) {
                return Some(u);
            }
        }
    }
    None
}

/// The first usage block anywhere below `v`.
fn find_usage(v: &Value) -> Option<Usage> {
    match v {
        Value::Object(map) => {
            if let Some(u) = usage_from_block(v) {
                return Some(u);
            }
            map.values().find_map(find_usage)
        }
        Value::Array(items) => items.iter().find_map(find_usage),
        _ => None,
    }
}

// ---------------- fields of a record ----------------

const MODEL_KEYS: &[&str] = &[
    "modelType",
    "model",
    "modelId",
    "model_id",
    "modelName",
    "model_name",
];
const CWD_KEYS: &[&str] = &[
    "cwd",
    "workingDirectory",
    "working_directory",
    "workspacePath",
    "workspace_path",
    "projectRoot",
    "project_root",
];
const TIME_KEYS: &[&str] = &[
    "createdAt",
    "created_at",
    "timestamp",
    "ts",
    "time",
    "startTime",
    "start_time",
    "lastUpdated",
    "lastUpdatedAt",
    "date",
];

/// The model a record was served by.
fn model_of(v: &Value) -> Option<String> {
    fn walk(v: &Value) -> Option<String> {
        match v {
            Value::Object(map) => {
                for key in MODEL_KEYS {
                    if let Some(s) = map.get(*key).and_then(|x| x.as_str()) {
                        if !s.trim().is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
                map.values().find_map(walk)
            }
            Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    walk(v)
}

/// The working directory a record was made in.
fn cwd_of(v: &Value) -> Option<String> {
    fn walk(v: &Value) -> Option<String> {
        match v {
            Value::Object(map) => {
                for key in CWD_KEYS {
                    if let Some(s) = map.get(*key).and_then(|x| x.as_str()) {
                        if !s.trim().is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
                map.values().find_map(walk)
            }
            Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    walk(v)
}

/// The day of an ISO-8601 timestamp, if it really looks like one.
fn iso_day(s: &str) -> Option<String> {
    let d = day_of(s);
    let b = d.as_bytes();
    if b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().all(|c| c.is_ascii_digit() || *c == b'-') {
        Some(d)
    } else {
        None
    }
}

/// The day of a unix timestamp in seconds.
fn unix_day_of(secs: i64) -> Option<String> {
    if secs <= 0 {
        None
    } else {
        Some(unix_day(secs))
    }
}

/// The day a record belongs to. Timestamps come as ISO strings, unix seconds
/// or unix milliseconds depending on the agent.
fn day_of_value(v: &Value) -> Option<String> {
    fn one(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => {
                if let Some(d) = iso_day(s) {
                    return Some(d);
                }
                unix_day_of(s.trim().parse::<i64>().ok()?)
            }
            Value::Number(n) => {
                let n = n.as_i64()?;
                unix_day_of(if n > 1_000_000_000_000 { n / 1000 } else { n })
            }
            _ => None,
        }
    }
    match v {
        Value::Object(map) => {
            for key in TIME_KEYS {
                if let Some(found) = map.get(*key).and_then(one) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(one),
        _ => None,
    }
}

/// True when the log marks the run as a delegated subagent.
fn subagent_of(v: &Value) -> bool {
    fn walk(v: &Value) -> bool {
        match v {
            Value::Object(map) => {
                for key in ["subagent", "isSubagent", "is_subagent", "subAgent"] {
                    if map.get(key).and_then(|x| x.as_bool()) == Some(true) {
                        return true;
                    }
                }
                map.values().any(walk)
            }
            Value::Array(items) => items.iter().any(walk),
            _ => false,
        }
    }
    walk(v)
}

// ---------------- one request ----------------

/// A request read out of a log.
struct Hit {
    usage: Usage,
    model: String,
    day: String,
    cwd: Option<String>,
    /// The cost the agent itself reported, in dollars.
    cost: Option<f64>,
    billing: Billing,
    subagent: bool,
}

impl Hit {
    fn new(usage: Usage, model: String, day: String, cwd: Option<String>, billing: Billing) -> Hit {
        Hit {
            usage,
            model: normalize_model(&model),
            day,
            cwd,
            cost: None,
            billing,
            subagent: false,
        }
    }
}

/// The row for a request, priced against the public list prices.
fn hit_row(agent: &str, hit: &Hit, pricing: &Pricing) -> Row {
    let mut r = Row {
        day: hit.day.clone(),
        agent: agent.to_string(),
        model: hit.model.clone(),
        requests: 1,
        input: hit.usage.input,
        cache_read: hit.usage.cache_read,
        cache_write: hit.usage.cache_write,
        output: hit.usage.output,
        reasoning: hit.usage.reasoning,
        project: label_for_cwd(hit.cwd.as_deref()),
        billing: hit.billing,
        subagent: hit.subagent,
        ..Default::default()
    };
    if let Some(c) = hit.cost {
        // the agent priced the request itself, so its number wins
        r.cost = c;
        r.cost_known = true;
    } else if let Some(p) = pricing.lookup(model_provider(&hit.model), &hit.model) {
        r.cost = p.cost(r.input, r.cache_read, r.cache_write, r.output);
        r.cost_known = true;
        r.savings = p.savings(r.cache_read, r.cache_write);
    }
    r
}

/// One request of a chat document, read out of the entry that carries it.
fn push_hit(hits: &mut Vec<Hit>, item: &Value, doc: &Value, billing: Billing) {
    let Some(usage) = find_usage(item) else { return };
    // an empty usage block is not a request
    if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
        return;
    }
    let model = model_of(item)
        .or_else(|| model_of(doc))
        .unwrap_or_else(|| "unknown".into());
    let day = day_of_value(item).or_else(|| day_of_value(doc)).unwrap_or_default();
    let cwd = cwd_of(item).or_else(|| cwd_of(doc));
    let mut hit = Hit::new(usage, model, day, cwd, billing);
    hit.subagent = subagent_of(item);
    hits.push(hit);
}

/// The requests of a chat document.
///
/// The agents keep the list of requests under different names: Cursor in
/// "conversation", Copilot in "exchanges", Gemini in "messages". Every entry is
/// one request with its own usage block.
fn records_of(doc: &Value, containers: &[&str], billing: Billing) -> Vec<Hit> {
    let mut hits = Vec::new();
    if let Some(arr) = doc.as_array() {
        for item in arr {
            push_hit(&mut hits, item, doc, billing);
        }
        if !hits.is_empty() {
            return hits;
        }
    }
    for key in containers {
        let Some(arr) = doc.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in arr {
            push_hit(&mut hits, item, doc, billing);
        }
        if !hits.is_empty() {
            return hits;
        }
    }
    // the document itself is one request
    if let Some(usage) = usage_from_block(doc).or_else(|| usage_total_only(doc)) {
        if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
            return hits;
        }
        let model = model_of(doc).unwrap_or_else(|| "unknown".into());
        let day = day_of_value(doc).unwrap_or_default();
        let cwd = cwd_of(doc);
        let mut hit = Hit::new(usage, model, day, cwd, billing);
        hit.subagent = subagent_of(doc);
        hits.push(hit);
    }
    hits
}

// ---------------- files ----------------

/// Every file under `root` with one of the given extensions.
fn walk_files(root: &Path, exts: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, exts, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if exts.contains(&ext) {
                    out.push(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    if root.exists() {
        walk(root, exts, &mut out);
    }
    out.sort();
    out
}

/// The top-level JSON objects inside a text blob.
///
/// Cursor keeps its chat state in sqlite blobs that wrap the JSON payload, so
/// the braces have to be counted by hand before anything is parsed.
fn json_objects(text: &str) -> Vec<Value> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut end = None;
        let mut j = i;
        while j < bytes.len() {
            let c = bytes[j];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        let Some(end) = end else {
            i += 1;
            continue;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&text[i..=end]) {
            out.push(v);
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// The documents of a log file: one whole json document, one per line for
/// jsonl, or the json objects embedded in a log line.
fn docs_of(file: &Path, text: &str, keys: &Prefilter) -> Vec<Value> {
    if keys.skips(text) {
        return Vec::new();
    }
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "jsonl" => text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || keys.skips(line) {
                    return None;
                }
                serde_json::from_str::<Value>(line).ok()
            })
            .collect(),
        "log" => {
            let mut out = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || keys.skips(line) {
                    continue;
                }
                out.extend(json_objects(line));
            }
            out
        }
        _ => match serde_json::from_str::<Value>(text) {
            Ok(v) => vec![v],
            // a file that is not one clean document: take the objects out of it
            Err(_) => json_objects(text),
        },
    }
}

/// The day the file was written. Used when the log itself carries no date.
fn mtime_day(file: &Path) -> String {
    fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| unix_day(d.as_secs() as i64))
        .unwrap_or_default()
}

/// Walk a list of files, replaying the cached rows of unchanged files and
/// parsing the rest.
fn scan_files(
    files: Vec<PathBuf>,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
    parse: impl Fn(&Path, &Pricing, &mut Rows, &mut Sess),
) -> u64 {
    let mut uncached = Vec::new();
    for file in &files {
        let path_str = file.to_string_lossy().to_string();
        if let Some((mtime_ns, size)) = file_stamp(file) {
            if let Some(entry) = cache.get_file(&path_str, mtime_ns, size) {
                for r in &entry.rows {
                    add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
                }
                for (day, agent) in &entry.sessions {
                    session(sess, agent, day);
                }
                continue;
            }
            uncached.push((file.clone(), path_str, mtime_ns, size));
        }
    }

    if !uncached.is_empty() {
        cache.dirty = true;
        for (file, path_str, mtime_ns, size) in uncached {
            let mut file_rows = Rows::new();
            let mut file_sess = Sess::new();
            parse(&file, pricing, &mut file_rows, &mut file_sess);
            // a file can hold several sessions on the same day, so the count is
            // kept by repeating the pair instead of storing it once
            let mut sessions = Vec::new();
            for ((day, agent), n) in &file_sess {
                for _ in 0..*n {
                    sessions.push((day.clone(), agent.clone()));
                }
            }
            let cached = CachedFile {
                mtime_ns,
                size,
                rows: file_rows.into_values().collect(),
                sessions,
                ..Default::default()
            };
            for r in &cached.rows {
                add(rows, sess, &r.agent, &r.model, &r.day, r.clone());
            }
            for (day, agent) in &cached.sessions {
                session(sess, agent, day);
            }
            cache.files.insert(path_str, cached);
        }
    }

    files.len() as u64
}

// ---------------- Cursor ----------------

/// The request containers of a Cursor chat document.
static CURSOR_CONTAINERS: &[&str] = &[
    "conversation",
    "messages",
    "bubbles",
    "exchanges",
    "requests",
    "turns",
    "responses",
    "history",
];

static CURSOR_KEYS: LazyLock<Prefilter> = LazyLock::new(|| {
    Prefilter::new(&[
        "usage",
        "tokenusage",
        "tokens",
        "stats",
        "inputtokens",
        "outputtokens",
        "totaltokens",
        "cachedinputtokens",
    ])
});

fn parse_cursor_file(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    if file.extension().and_then(|e| e.to_str()) == Some("db") {
        parse_cursor_db(file, pricing, rows, sess);
        return;
    }
    let Ok(text) = fs::read_to_string(file) else { return };
    let docs = docs_of(file, &text, &CURSOR_KEYS);
    let mtime = mtime_day(file);
    let mut session_day = String::new();
    for doc in &docs {
        for hit in records_of(doc, CURSOR_CONTAINERS, Billing::Api) {
            let mut hit = hit;
            if hit.day.is_empty() {
                hit.day = mtime.clone();
            }
            if hit.day.is_empty() {
                continue;
            }
            if session_day.is_empty() || hit.day < session_day {
                session_day = hit.day.clone();
            }
            let r = hit_row(CURSOR, &hit, pricing);
            add(rows, sess, CURSOR, &hit.model, &hit.day, r);
        }
    }
    // one conversation file is one session
    if session_day.is_empty() {
        session_day = mtime;
    }
    if !session_day.is_empty() {
        session(sess, CURSOR, &session_day);
    }
}

/// The newer Cursor builds keep the chat in a sqlite file next to the
/// conversation. The rows hold the conversation as a blob, so the json payload
/// is picked out of the blob.
fn parse_cursor_db(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    let Ok(conn) = rusqlite::Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let Ok(mut stmt) = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'") else {
        return;
    };
    let Ok(it) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return;
    };
    let tables: Vec<String> = it.flatten().collect();

    let mtime = mtime_day(file);
    let mut session_day = String::new();
    for table in tables {
        // the table names come out of the file, so only plain identifiers are
        // allowed back into the query
        if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Ok(mut stmt) = conn.prepare(&format!("SELECT * FROM \"{}\"", table)) else {
            continue;
        };
        let columns = stmt.column_count();
        let Ok(mut iter) = stmt.query([]) else { continue };
        loop {
            let row = match iter.next() {
                Ok(Some(r)) => r,
                _ => break,
            };
            for i in 0..columns {
                let Ok(val) = row.get_ref(i) else { continue };
                let text = match val {
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                    _ => continue,
                };
                if text.is_empty() || CURSOR_KEYS.skips(&text) {
                    continue;
                }
                for doc in json_objects(&text) {
                    for hit in records_of(&doc, CURSOR_CONTAINERS, Billing::Api) {
                        let mut hit = hit;
                        if hit.day.is_empty() {
                            hit.day = mtime.clone();
                        }
                        if hit.day.is_empty() {
                            continue;
                        }
                        if session_day.is_empty() || hit.day < session_day {
                            session_day = hit.day.clone();
                        }
                        let r = hit_row(CURSOR, &hit, pricing);
                        add(rows, sess, CURSOR, &hit.model, &hit.day, r);
                    }
                }
            }
        }
    }

    if session_day.is_empty() {
        session_day = mtime;
    }
    if !session_day.is_empty() {
        session(sess, CURSOR, &session_day);
    }
}

/// The chats of the editor and the chats of the cli.
pub fn parse_cursor(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let root = home.join(".cursor");
    if !root.exists() {
        return 0;
    }
    let mut files = Vec::new();
    for dir in [root.join("chats"), root.join("cli-chats")] {
        files.extend(walk_files(&dir, &["json", "jsonl", "db"]));
    }
    if files.is_empty() {
        return 0;
    }
    scan_files(files, pricing, cache, rows, sess, parse_cursor_file)
}

// ---------------- GitHub Copilot ----------------

/// The request containers of a copilot session state file.
static COPILOT_CONTAINERS: &[&str] = &["exchanges", "requests", "messages", "turns", "responses"];

static COPILOT_KEYS: LazyLock<Prefilter> = LazyLock::new(|| {
    Prefilter::new(&[
        "usage",
        "tokens",
        "tokenusage",
        "stats",
        "inputtokens",
        "outputtokens",
        "totaltokens",
    ])
});

fn parse_copilot_file(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    let Ok(text) = fs::read_to_string(file) else { return };
    let docs = docs_of(file, &text, &COPILOT_KEYS);
    let mtime = mtime_day(file);
    let mut session_day = String::new();
    for doc in &docs {
        for hit in records_of(doc, COPILOT_CONTAINERS, Billing::Api) {
            let mut hit = hit;
            if hit.day.is_empty() {
                hit.day = mtime.clone();
            }
            if hit.day.is_empty() {
                continue;
            }
            if session_day.is_empty() || hit.day < session_day {
                session_day = hit.day.clone();
            }
            let r = hit_row(COPILOT, &hit, pricing);
            add(rows, sess, COPILOT, &hit.model, &hit.day, r);
        }
    }
    // a session state file is one cli session, the log files are not
    if file.extension().and_then(|e| e.to_str()) == Some("json")
        && file
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n == "history-session-state")
            .unwrap_or(false)
    {
        if session_day.is_empty() {
            session_day = mtime;
        }
        if !session_day.is_empty() {
            session(sess, COPILOT, &session_day);
        }
    }
}

/// The session states, the cli logs and the config directory of the copilot
/// cli.
pub fn parse_copilot(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let mut files = Vec::new();
    files.extend(walk_files(
        &home.join(".copilot").join("history-session-state"),
        &["json"],
    ));
    files.extend(walk_files(&home.join(".copilot").join("logs"), &["log"]));
    files.extend(walk_files(
        &home.join(".config").join("github-copilot"),
        &["json", "jsonl", "log"],
    ));
    if files.is_empty() {
        return 0;
    }
    scan_files(files, pricing, cache, rows, sess, parse_copilot_file)
}

// ---------------- Gemini CLI ----------------

/// The request containers of a gemini log or chat file.
static GEMINI_CONTAINERS: &[&str] = &[
    "records",
    "entries",
    "messages",
    "events",
    "exchanges",
    "history",
    "requests",
];

static GEMINI_KEYS: LazyLock<Prefilter> = LazyLock::new(|| {
    Prefilter::new(&[
        "usage",
        "tokens",
        "tokenusage",
        "stats",
        "inputtokens",
        "outputtokens",
        "totaltokens",
        "cached",
    ])
});

/// The working directory the gemini cli recorded next to the session.
///
/// The cli writes the project root of a session into a ".project_root" file in
/// the session directory, so the chats themselves carry no path.
fn gemini_project_root(file: &Path) -> Option<String> {
    let mut dir = file.parent();
    for _ in 0..3 {
        let d = dir?;
        let marker = d.join(".project_root");
        if marker.is_file() {
            if let Ok(text) = fs::read_to_string(&marker) {
                let t = text.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        dir = d.parent();
    }
    None
}

fn parse_gemini_file(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    let Ok(text) = fs::read_to_string(file) else { return };
    let docs = docs_of(file, &text, &GEMINI_KEYS);
    let mtime = mtime_day(file);
    let root = gemini_project_root(file);
    let mut session_day = String::new();
    let mut requests = 0u64;
    for doc in &docs {
        for hit in records_of(doc, GEMINI_CONTAINERS, Billing::Unknown) {
            let mut hit = hit;
            if hit.cwd.is_none() {
                hit.cwd = root.clone();
            }
            if hit.day.is_empty() {
                hit.day = mtime.clone();
            }
            if hit.day.is_empty() {
                continue;
            }
            if session_day.is_empty() || hit.day < session_day {
                session_day = hit.day.clone();
            }
            requests += 1;
            let r = hit_row(GEMINI, &hit, pricing);
            add(rows, sess, GEMINI, &hit.model, &hit.day, r);
        }
    }
    // a chat file is one session, the telemetry log only counts when it has
    // something in it
    let is_chat = file
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == "chats")
        .unwrap_or(false);
    if is_chat || requests > 0 {
        if session_day.is_empty() {
            session_day = mtime;
        }
        if !session_day.is_empty() {
            session(sess, GEMINI, &session_day);
        }
    }
}

/// The logs and the chats of the gemini cli.
pub fn parse_gemini(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let root = home.join(".gemini");
    if !root.exists() {
        return 0;
    }
    let mut files = Vec::new();
    files.extend(walk_files(&root.join("tmp"), &["json", "jsonl"]));
    files.extend(walk_files(&root.join("sessions"), &["json", "jsonl"]));
    if files.is_empty() {
        return 0;
    }
    scan_files(files, pricing, cache, rows, sess, parse_gemini_file)
}

// ---------------- Cline ----------------

/// The cline extension keeps its data in the global storage of the editor.
fn cline_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let roaming = home.join("AppData").join("Roaming");
    for base in [
        "Code",
        "Code - Insiders",
        "Cursor",
        "Cursor - Insiders",
        "VSCodium",
        "VSCodium - Insiders",
    ] {
        for prefix in [roaming.clone(), home.join(".config")] {
            roots.push(
                prefix
                    .join(base)
                    .join("User")
                    .join("globalStorage")
                    .join("saoudrizwan.claude-dev"),
            );
        }
    }
    roots.push(home.join(".cline"));
    roots
}

static CLINE_KEYS: LazyLock<Prefilter> = LazyLock::new(|| {
    Prefilter::new(&[
        "tokens",
        "usage",
        "stats",
        "historyitem",
        "taskhistory",
        "apiconversationhistory",
        "tokenusage",
    ])
});

/// The token counts of a cline task or message.
///
/// Cline counts the tokens per task: "tokensIn"/"tokensOut" on the task state,
/// or a single "tokensUsed" when only the total was kept.
fn cline_usage(v: &Value) -> Option<Usage> {
    let obj = v.as_object()?;
    let tin = obj.get("tokensIn").and_then(|x| x.as_u64());
    let tout = obj.get("tokensOut").and_then(|x| x.as_u64());
    if tin.is_some() || tout.is_some() {
        return Some(Usage {
            input: tin.unwrap_or(0),
            output: tout.unwrap_or(0),
            ..Default::default()
        });
    }
    if let Some(total) = obj.get("tokensUsed").and_then(|x| x.as_u64()) {
        return Some(Usage {
            input: total,
            ..Default::default()
        });
    }
    usage_from_block(v)
}

/// One cline task or message.
fn cline_hit(item: &Value) -> Option<Hit> {
    let usage = cline_usage(item)?;
    if usage.input == 0 && usage.output == 0 && usage.cache_read == 0 && usage.cache_write == 0 {
        return None;
    }
    let model = model_of(item).unwrap_or_else(|| "unknown".into());
    let day = day_of_value(item).unwrap_or_default();
    let cwd = cwd_of(item);
    // cline prices the task itself, so that number is used when it is there
    let cost = item
        .get("totalCost")
        .or_else(|| item.get("cost"))
        .and_then(|c| c.as_f64())
        .filter(|c| *c > 0.0);
    let mut hit = Hit::new(usage, model, day, cwd, Billing::Unknown);
    hit.cost = cost;
    hit.subagent = subagent_of(item);
    Some(hit)
}

fn parse_cline_file(file: &Path, pricing: &Pricing, rows: &mut Rows, sess: &mut Sess) {
    let Ok(text) = fs::read_to_string(file) else { return };
    let docs = docs_of(file, &text, &CLINE_KEYS);
    let mtime = mtime_day(file);
    for doc in &docs {
        // the task history of the extension: one entry per finished task
        if let Some(arr) = doc.get("taskHistory").and_then(|v| v.as_array()) {
            for item in arr {
                let Some(mut hit) = cline_hit(item) else { continue };
                if hit.day.is_empty() {
                    hit.day = mtime.clone();
                }
                let r = hit_row(CLINE, &hit, pricing);
                add(rows, sess, CLINE, &hit.model, &hit.day, r);
                if !hit.day.is_empty() {
                    session(sess, CLINE, &hit.day);
                }
            }
            continue;
        }
        // the state file of a single task
        if let Some(item) = doc.get("historyItem") {
            let Some(mut hit) = cline_hit(item) else { continue };
            if hit.day.is_empty() {
                hit.day = mtime.clone();
            }
            let r = hit_row(CLINE, &hit, pricing);
            add(rows, sess, CLINE, &hit.model, &hit.day, r);
            if !hit.day.is_empty() {
                session(sess, CLINE, &hit.day);
            }
            continue;
        }
        // the api conversation of a task: the messages carry their own counts
        if let Some(arr) = doc.get("apiConversationHistory").and_then(|v| v.as_array()) {
            for item in arr {
                let Some(mut hit) = cline_hit(item) else { continue };
                if hit.day.is_empty() {
                    hit.day = mtime.clone();
                }
                let r = hit_row(CLINE, &hit, pricing);
                add(rows, sess, CLINE, &hit.model, &hit.day, r);
            }
            continue;
        }
        // anything else that carries counts
        if let Some(mut hit) = cline_hit(doc) {
            if hit.day.is_empty() {
                hit.day = mtime.clone();
            }
            let r = hit_row(CLINE, &hit, pricing);
            add(rows, sess, CLINE, &hit.model, &hit.day, r);
            if !hit.day.is_empty() {
                session(sess, CLINE, &hit.day);
            }
        }
    }
}

/// The global storage of the cline extension and the cline home directory.
pub fn parse_cline(
    home: &Path,
    pricing: &Pricing,
    cache: &mut ScanCache,
    rows: &mut Rows,
    sess: &mut Sess,
) -> u64 {
    let mut files = Vec::new();
    for root in cline_roots(home) {
        files.extend(walk_files(&root, &["json"]));
    }
    if files.is_empty() {
        return 0;
    }
    files.sort();
    files.dedup();
    scan_files(files, pricing, cache, rows, sess, parse_cline_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fake home directory that is removed when the test ends.
    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cinder-agents-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run(home: &Home, f: impl FnOnce(&Path, &Pricing, &mut ScanCache, &mut Rows, &mut Sess)) -> (Rows, Sess) {
        let pricing = Pricing::load(&home.0);
        let mut cache = ScanCache::load_for(&home.0, 0);
        let mut rows = Rows::new();
        let mut sess = Sess::new();
        f(&home.0, &pricing, &mut cache, &mut rows, &mut sess);
        (rows, sess)
    }

    fn row_of(rows: &Rows, model: &str) -> Row {
        rows.values()
            .find(|r| r.model == model)
            .cloned()
            .unwrap_or_else(|| panic!("no row for {}", model))
    }

    #[test]
    fn cursor_chats_carry_the_token_counts_of_every_bubble() {
        let home = Home::new();
        home.write(
            ".cursor/chats/abc/conv.json",
            &json!({
                "createdAt": "2026-01-02T10:00:00Z",
                "modelType": "claude-3-5-sonnet-20241022",
                "cwd": home.0.join("proj").to_string_lossy(),
                "conversation": [
                    {"createdAt": "2026-01-02T10:00:00Z", "modelType": "gpt-4o",
                     "tokenUsage": {"inputTokens": 100, "outputTokens": 20,
                                    "cacheWritesTokens": 5, "cacheReadsTokens": 7, "totalTokens": 132}}
                ]
            })
            .to_string(),
        );
        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            let n = parse_cursor(home, pricing, cache, rows, sess);
            assert_eq!(n, 1);
        });
        let row = row_of(&rows, "gpt-4o");
        assert_eq!(row.agent, "Cursor");
        assert_eq!(row.day, "2026-01-02");
        assert_eq!((row.requests, row.input, row.output), (1, 100, 20));
        assert_eq!((row.cache_read, row.cache_write), (7, 5));
        assert_eq!(row.billing, Billing::Api);
        assert_eq!(sess.get(&("2026-01-02".into(), "Cursor".into())), Some(&1));
    }

    #[test]
    fn a_missing_cursor_leaves_nothing_behind() {
        let home = Home::new();
        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            assert_eq!(parse_cursor(home, pricing, cache, rows, sess), 0);
        });
        assert!(rows.is_empty());
        assert!(sess.is_empty());
        assert!(!home.0.join(".cursor").exists());
    }

    #[test]
    fn the_newer_cursor_chats_are_read_out_of_the_store_database() {
        let home = Home::new();
        let db = home.0.join(".cursor/chats/hash/uuid/store.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE blobs (key TEXT, value TEXT);").unwrap();
        conn.execute(
            "INSERT INTO blobs VALUES ('composer', ?)",
            [json!({
                "composerId": "c1",
                "createdAt": "2026-01-05T00:00:00Z",
                "modelType": "gpt-4o",
                "conversation": [
                    {"createdAt": "2026-01-05T00:00:00Z", "modelType": "gpt-4o",
                     "tokenUsage": {"inputTokens": 11, "outputTokens": 3}}
                ]
            })
            .to_string()],
        )
        .unwrap();
        drop(conn);

        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            assert_eq!(parse_cursor(home, pricing, cache, rows, sess), 1);
        });
        let row = row_of(&rows, "gpt-4o");
        assert_eq!((row.input, row.output), (11, 3));
        assert_eq!(row.day, "2026-01-05");
        assert_eq!(sess.get(&("2026-01-05".into(), "Cursor".into())), Some(&1));
    }

    #[test]
    fn a_usage_block_with_only_a_total_keeps_the_token_count() {
        let home = Home::new();
        home.write(
            ".copilot/history-session-state/s1.state.json",
            &json!({
                "exchanges": [
                    {"timestamp": "2026-01-03T08:00:00Z",
                     "response": {"model": "gpt-4o", "usage": {"totalTokens": 70}}}
                ]
            })
            .to_string(),
        );
        let (rows, _sess) = run(&home, |home, pricing, cache, rows, sess| {
            parse_copilot(home, pricing, cache, rows, sess);
        });
        let row = row_of(&rows, "gpt-4o");
        assert_eq!(row.input, 70);
        assert_eq!(row.output, 0);
    }

    #[test]
    fn copilot_session_states_carry_the_model_and_the_usage() {
        let home = Home::new();
        home.write(
            ".copilot/history-session-state/s1.state.json",
            &json!({
                "sessionId": "s1",
                "exchanges": [
                    {"timestamp": "2026-01-03T08:00:00Z",
                     "response": {"model": "gpt-4o",
                                  "usage": {"prompt_tokens": 100, "completion_tokens": 30,
                                            "total_tokens": 130,
                                            "prompt_tokens_details": {"cached_tokens": 10}}}}
                ]
            })
            .to_string(),
        );
        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            assert_eq!(parse_copilot(home, pricing, cache, rows, sess), 1);
        });
        let row = row_of(&rows, "gpt-4o");
        assert_eq!(row.agent, "GitHub Copilot");
        assert_eq!((row.input, row.output, row.cache_read), (100, 30, 10));
        assert_eq!(row.day, "2026-01-03");
        assert_eq!(sess.get(&("2026-01-03".into(), "GitHub Copilot".into())), Some(&1));
    }

    #[test]
    fn gemini_logs_and_chats_carry_their_token_counts() {
        let home = Home::new();
        home.write(
            ".gemini/tmp/abc/logs.json",
            &json!([{
                "timestamp": "2026-01-04T09:00:00Z",
                "type": "gemini-api-call",
                "payload": {"model": "gemini-2.5-pro",
                            "stats": {"input": 40, "output": 8, "cached": 2, "total": 50}}
            }])
            .to_string(),
        );
        home.write(
            ".gemini/tmp/abc/chats/session-1.json",
            &json!({
                "sessionId": "s",
                "startTime": "2026-01-04T09:00:00Z",
                "messages": [
                    {"role": "user", "timestamp": "2026-01-04T09:00:00Z",
                     "tokens": {"input": 10, "output": 0, "cached": 0, "total": 10}},
                    {"role": "model", "timestamp": "2026-01-04T09:01:00Z",
                     "tokens": {"input": 20, "output": 30, "cached": 5, "total": 55}}
                ]
            })
            .to_string(),
        );
        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            assert_eq!(parse_gemini(home, pricing, cache, rows, sess), 2);
        });
        let row = row_of(&rows, "gemini-2.5-pro");
        assert_eq!(row.agent, "Gemini CLI");
        assert_eq!((row.input, row.output, row.cache_read), (40, 8, 2));
        assert_eq!(row.billing, Billing::Unknown);
        // the chat file adds a second request on the same model
        let total: u64 = rows.values().map(|r| r.input).sum();
        assert_eq!(total, 70);
        assert_eq!(sess.get(&("2026-01-04".into(), "Gemini CLI".into())), Some(&2));
    }

    #[test]
    fn the_gemini_project_root_file_names_the_project() {
        let home = Home::new();
        let proj = home.0.join("gemwork");
        fs::create_dir_all(&proj).unwrap();
        home.write(".gemini/tmp/abc/.project_root", &proj.to_string_lossy());
        home.write(
            ".gemini/tmp/abc/chats/session-1.json",
            &json!({
                "sessionId": "s",
                "messages": [
                    {"role": "model", "timestamp": "2026-01-04T09:01:00Z",
                     "tokens": {"input": 20, "output": 30, "cached": 5, "total": 55}}
                ]
            })
            .to_string(),
        );
        let (rows, _sess) = run(&home, |home, pricing, cache, rows, sess| {
            parse_gemini(home, pricing, cache, rows, sess);
        });
        let row = rows.values().next().unwrap();
        assert_eq!(row.project, "gemwork");
        assert_eq!(row.input, 20);
    }

    #[test]
    fn cline_task_states_carry_the_tokens_the_cost_and_the_project() {
        let home = Home::new();
        let proj = home.0.join("proj");
        fs::create_dir_all(&proj).unwrap();
        home.write(
            "AppData/Roaming/Code/User/globalStorage/saoudrizwan.claude-dev/tasks/t1/state.json",
            &json!({
                "id": "t1",
                "historyItem": {"id": "t1", "ts": 1767225600000i64, "task": "do it",
                                "tokensIn": 100, "tokensOut": 25, "totalCost": 0.02,
                                "cwd": proj.to_string_lossy(), "apiProvider": "openai",
                                "model": "gpt-4o"}
            })
            .to_string(),
        );
        let (rows, sess) = run(&home, |home, pricing, cache, rows, sess| {
            assert_eq!(parse_cline(home, pricing, cache, rows, sess), 1);
        });
        let row = row_of(&rows, "gpt-4o");
        assert_eq!(row.agent, "Cline");
        assert_eq!((row.input, row.output), (100, 25));
        assert_eq!(row.cost, 0.02);
        assert!(row.cost_known);
        assert_eq!(row.day, "2026-01-01");
        assert_eq!(row.project, "proj");
        assert_eq!(sess.get(&("2026-01-01".into(), "Cline".into())), Some(&1));
    }

    #[test]
    fn a_second_scan_replays_the_cached_rows() {
        let home = Home::new();
        home.write(
            ".copilot/history-session-state/s1.state.json",
            &json!({
                "exchanges": [
                    {"timestamp": "2026-01-03T08:00:00Z",
                     "response": {"model": "gpt-4o", "usage": {"prompt_tokens": 100}}}
                ]
            })
            .to_string(),
        );
        let pricing = Pricing::load(&home.0);
        let mut cache = ScanCache::load_for(&home.0, 0);
        let mut rows = Rows::new();
        let mut sess = Sess::new();
        parse_copilot(&home.0, &pricing, &mut cache, &mut rows, &mut sess);
        assert_eq!(row_of(&rows, "gpt-4o").input, 100);

        // the file did not change, so the second scan reads the cache
        let mut rows2 = Rows::new();
        let mut sess2 = Sess::new();
        parse_copilot(&home.0, &pricing, &mut cache, &mut rows2, &mut sess2);
        assert_eq!(row_of(&rows2, "gpt-4o").input, 100);
        assert_eq!(sess2.get(&("2026-01-03".into(), "GitHub Copilot".into())), Some(&1));
    }
}
