use super::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cinder-test-{}-{}-{}",
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

    fn lines(&self, path: &str, lines: &[Value]) {
        self.write(path, &lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
    }

    fn db(&self, path: &str) -> rusqlite::Connection {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        rusqlite::Connection::open(path).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_prices_include_both_catalogs_and_both_pi_formats() {
    let home = Home::new();
    home.write(".pi/agent/models-store.json", &json!({
        "vendor": {"models": [{"id": "audit-only", "cost": {"input": 2, "output": 3}}]},
        "other": {"models": {"audit-only": {"cost": {"input": 2, "output": 3}}}}
    }).to_string());
    home.write(".pi/agent/clinepass-prices.json", &json!({"models": {
        "cline/audit-cline": {"input": 4, "output": 5},
        "bare-cline": {"input": 6, "output": 7}
    }}).to_string());
    let p = Pricing::load(&home.0);
    assert_eq!(p.lookup(Some("vendor"), "audit-only").unwrap().i, 2.0);
    assert_eq!(p.lookup(Some("other"), "audit-only").unwrap().o, 3.0);
    assert_eq!(p.lookup(None, "audit-only").unwrap().i, 2.0);
    assert_eq!(p.lookup(Some("cline"), "audit-cline").unwrap().i, 4.0);
    assert_eq!(p.lookup(None, "audit-cline").unwrap().o, 5.0);
    assert_eq!(p.lookup(None, "bare-cline").unwrap().i, 6.0);
    assert_eq!(p.local_count, 4);
}

#[test]
fn local_price_conflicts_include_cache_prices_and_do_not_pick_a_provider() {
    let home = Home::new();
    home.write(".pi/agent/models-store.json", &json!({
        "openai": {"models": [{"id": "gpt-5", "cost": {"input": 2, "output": 3, "cacheRead": 0.2}}]},
        "other": {"models": [{"id": "gpt-5", "cost": {"input": 2, "output": 3, "cacheRead": 1}}]}
    }).to_string());
    let p = Pricing::load(&home.0);
    assert!(p.lookup(None, "gpt-5").is_none());
    assert!(p.lookup(None, "gpt-5-1m").is_none());
    assert_eq!(p.lookup(Some("openai"), "gpt-5").unwrap().cr, 0.2);
    assert_eq!(p.lookup(Some("other"), "gpt-5").unwrap().cr, 1.0);
}

#[test]
fn pi_prices_override_cline_and_provider_model_paths_still_match() {
    let home = Home::new();
    home.write(".pi/agent/clinepass-prices.json", &json!({"models": {
        "openrouter/vendor/audit": {"input": 9, "output": 9}
    }}).to_string());
    home.write(".pi/agent/models-store.json", &json!({"openrouter": {"models": [
        {"id": "vendor/audit", "cost": {"input": 0, "output": 0}}
    ]}}).to_string());
    let p = Pricing::load(&home.0);
    assert_eq!(p.lookup(Some("openrouter"), "vendor/audit").unwrap().i, 0.0);
    assert_eq!(p.lookup(Some("openrouter"), "openrouter/vendor/audit").unwrap().o, 0.0);
}

#[test]
fn unicode_model_names_do_not_panic_and_context_variants_still_match() {
    let home = Home::new();
    home.write(".pi/agent/models-store.json", &json!({"vendor": {"models": [
        {"id": "audit-model", "cost": {"input": 2, "output": 3}}
    ]}}).to_string());
    let p = Pricing::load(&home.0);
    for model in ["custom-é", "custom-模型", "custom-🦀", "custom-m"] {
        assert!(p.lookup(Some("vendor"), model).is_none());
    }
    assert_eq!(p.lookup(Some("vendor"), "audit-model-1m").unwrap().i, 2.0);
}

#[test]
fn scans_keep_codex_cache_counts_as_reported() {
    let home = Home::new();
    home.lines(".pi/agent/sessions/pi.jsonl", &[
        json!({"type": "session", "timestamp": "2026-01-01T00:00:00Z"}),
        json!({"type": "message", "timestamp": "2026-01-01T00:00:00Z", "message": {
            "role": "assistant", "model": "gpt-5", "provider": "openai",
            "usage": {"input": 5000, "cacheRead": 95000, "output": 10}
        }})
    ]);
    home.lines(".codex/sessions/codex.jsonl", &[
        json!({"type": "session_meta", "timestamp": "2026-01-01T00:00:00Z"}),
        json!({"type": "turn_context", "payload": {"model": "gpt-5"}}),
        json!({"type": "event_msg", "timestamp": "2026-01-01T00:00:00Z", "payload": {
            "type": "token_count", "info": {"last_token_usage": {
                "input_tokens": 100000, "cached_input_tokens": 0, "output_tokens": 10
            }}
        }}),
        json!({"type": "event_msg", "timestamp": "2026-01-01T00:01:00Z", "payload": {
            "type": "token_count", "info": {"last_token_usage": {
                "input_tokens": 200000, "cached_input_tokens": 50000, "output_tokens": 20
            }}
        }})
    ]);
    let stats = scan_home(&home.0, &|_| {});
    let row = stats.rows.iter().find(|r| r.agent == "Codex").unwrap();
    assert_eq!(row.requests, 2);
    assert_eq!(row.input, 250000);
    assert_eq!(row.cache_read, 50000);
    assert_eq!(row.output, 30);
}

#[test]
fn codex_limits_keep_both_latest_windows_after_a_reset() {
    let home = Home::new();
    let event = |ts, pct, plan, reset| json!({"type": "event_msg", "timestamp": ts, "payload": {
        "type": "token_count", "rate_limits": {"plan_type": plan,
            "primary": {"used_percent": pct, "window_minutes": 300, "resets_at": reset},
            "secondary": {"used_percent": pct + 1, "window_minutes": 10080, "resets_at": reset}
        }
    }});
    home.lines(".codex/sessions/a-new.jsonl", &[
        event("2026-01-02T01:00:00Z", 2, "plus", 200),
        event("2026-01-01T01:00:00Z", 90, "pro", 100)
    ]);
    home.lines(".codex/archived_sessions/z-old.jsonl", &[
        event("2026-01-01T23:00:00Z", 99, "pro", 100)
    ]);
    let stats = scan_home(&home.0, &|_| {});
    assert_eq!(stats.limits.len(), 2);
    assert_eq!(stats.limits[0].window, "primary");
    assert_eq!(stats.limits[0].used_percent, 2.0);
    assert_eq!(stats.limits[1].window, "secondary");
    assert_eq!(stats.limits[1].used_percent, 3.0);
    for limit in stats.limits {
        assert_eq!(limit.plan, "plus");
        assert_eq!(limit.resets_at, 200);
    }
}

fn t3_usage_db(home: &Home) -> rusqlite::Connection {
    let db = home.db(".t3/userdata/statev2.sqlite");
    db.execute_batch("CREATE TABLE orchestration_v2_projection_provider_turns (
        provider_thread_id TEXT, started_at TEXT, payload_json TEXT, run_attempt_id TEXT);
        CREATE TABLE orchestration_v2_projection_run_attempts (attempt_id TEXT, run_id TEXT);
        CREATE TABLE orchestration_v2_projection_runs (run_id TEXT, payload_json TEXT);
        INSERT INTO orchestration_v2_projection_run_attempts VALUES ('a', 'r');").unwrap();
    db.execute("INSERT INTO orchestration_v2_projection_runs VALUES ('r', ?)", [
        json!({"modelSelection": {"model": "antigravity/gemini-2.5-pro"}}).to_string()
    ]).unwrap();
    db
}

fn t3_turn(db: &rusqlite::Connection, id: &str) {
    db.execute("INSERT INTO orchestration_v2_projection_provider_turns VALUES (?, '2026-01-01T00:00:00Z', ?, 'a')",
        [id, &json!({"tokenUsage": {"inputTokens": 100, "outputTokens": 10}}).to_string()]).unwrap();
}

#[test]
fn t3_transcripts_supply_counts_and_usage_database_supplies_tokens() {
    let home = Home::new();
    home.lines(".t3/userdata/providers/antigravity/instance/antigravity-acp/brain/conversation/.system_generated/logs/transcript.jsonl", &[
        json!({"type": "PLANNER_RESPONSE", "created_at": "2026-01-01T00:00:00Z", "step_index": 1, "tool_calls": [{"id": "tool"}]}),
        json!({"type": "PLANNER_RESPONSE", "created_at": "2026-01-01T00:01:00Z", "step_index": 2}),
        json!({"type": "PLANNER_RESPONSE", "created_at": "2026-01-01T00:01:00Z", "step_index": 2})
    ]);
    let db = t3_usage_db(&home);
    t3_turn(&db, "conversation");
    t3_turn(&db, "without-transcript");
    let stats = scan_home(&home.0, &|_| {});
    assert_eq!(stats.rows.iter().map(|r| r.requests).sum::<u64>(), 3);
    assert_eq!(stats.rows.iter().map(|r| r.tools).sum::<u64>(), 1);
    assert_eq!(stats.rows.iter().map(Row::tokens).sum::<u64>(), 220);
    assert_eq!(stats.sessions.iter().map(|s| s.sessions).sum::<u64>(), 2);
    assert!(stats.sessions.iter().all(|s| s.day == "2026-01-01"));
}

#[test]
fn empty_or_broken_t3_transcripts_do_not_hide_usage_database_requests() {
    for body in ["", "{broken JSON}"] {
        let home = Home::new();
        home.write(".t3/userdata/providers/antigravity/instance/antigravity-acp/brain/conversation/.system_generated/logs/transcript.jsonl", body);
        let db = t3_usage_db(&home);
        t3_turn(&db, "conversation");
        let stats = scan_home(&home.0, &|_| {});
        assert_eq!(stats.rows.iter().map(|r| r.requests).sum::<u64>(), 1);
        assert_eq!(stats.sessions.iter().map(|s| s.sessions).sum::<u64>(), 1);
        assert_eq!(stats.rows.iter().map(Row::tokens).sum::<u64>(), 110);
    }
}

#[test]
fn t3_conversation_database_fallback_is_not_counted_again_by_usage() {
    let home = Home::new();
    let conv = home.db(".t3/userdata/providers/antigravity/instance/antigravity-acp/conversations/conversation.db");
    conv.execute_batch("CREATE TABLE steps (idx INTEGER, step_type INTEGER, metadata BLOB);
        INSERT INTO steps VALUES (1, 15, X''), (2, 21, X'');").unwrap();
    let db = t3_usage_db(&home);
    t3_turn(&db, "conversation");
    let stats = scan_home(&home.0, &|_| {});
    assert_eq!(stats.rows.iter().map(|r| r.requests).sum::<u64>(), 1);
    assert_eq!(stats.rows.iter().map(|r| r.tools).sum::<u64>(), 1);
    assert_eq!(stats.rows.iter().map(Row::tokens).sum::<u64>(), 110);
    assert_eq!(stats.sessions.iter().map(|s| s.sessions).sum::<u64>(), 1);
}

#[test]
fn opencode_uses_message_dates_models_and_request_counts() {
    for table in ["session", "session_v2"] {
        let home = Home::new();
        let db = home.db(".local/share/opencode/opencode.db");
        db.execute_batch(&format!("CREATE TABLE {table} (id TEXT, time_created INTEGER, time_updated INTEGER);
            INSERT INTO {table} VALUES ('s', 1767225600000, 1767398400000);
            CREATE TABLE message (id TEXT, session_id TEXT, data TEXT, time_created INTEGER);
            CREATE TABLE part (message_id TEXT, data TEXT, time_created INTEGER);
            INSERT INTO part VALUES ('a', '{{ \"type\": \"tool\" }}', 1767225600000),
                ('b', '{{\"type\":\"tool\"}}', 1767312000000),
                ('b', '{{\"type\":\"text\"}}', 1767312000000);")).unwrap();
        for (id, model, ts, input, cost) in [
            ("a", "gpt-5", 1767225600000_i64, 100, 1.0),
            ("a2", "gpt-5", 1767225601000, 50, 0.0),
            ("b", "gpt-4.1", 1767312000000, 200, 2.0)
        ] {
            db.execute("INSERT INTO message VALUES (?, 's', ?, ?)", rusqlite::params![id,
                json!({"role": "assistant", "providerID": "openai", "modelID": model,
                    "tokens": {"input": input, "output": 10, "cache": {"read": 20, "write": 5}}, "cost": cost}).to_string(), ts]).unwrap();
        }
        db.execute("INSERT INTO message VALUES ('user', 's', ?, 1767225600000)", [json!({"role": "user"}).to_string()]).unwrap();
        db.execute("INSERT INTO message VALUES ('bad', 's', 'not JSON', 1767225600000)", []).unwrap();
        let stats = scan_home(&home.0, &|_| {});
        let first = stats.rows.iter().find(|r| r.day == "2026-01-01").unwrap();
        assert_eq!(first.model, "openai/gpt-5");
        assert_eq!((first.requests, first.input, first.cache_read, first.cache_write, first.tools), (2, 150, 40, 10, 1));
        assert_eq!(first.cost, 1.0);
        let second = stats.rows.iter().find(|r| r.day == "2026-01-02").unwrap();
        assert_eq!(second.model, "openai/gpt-4.1");
        assert_eq!((second.requests, second.input, second.tools), (1, 200, 1));
        assert_eq!(stats.sessions.len(), 1);
        assert_eq!(stats.sessions[0].day, "2026-01-01");
        assert_eq!(stats.sessions[0].sessions, 1);
        assert!(!stats.rows.iter().any(|r| r.day == "2026-01-03"));
    }
}

#[test]
fn partial_costs_survive_aggregation_in_either_order_but_tools_do_not_poison_them() {
    for reverse in [false, true] {
        let mut rows = Rows::new();
        let mut sess = Sess::new();
        let mut input = vec![
            Row { input: 100, cost: 1.0, cost_known: true, ..Default::default() },
            Row { input: 100, ..Default::default() }
        ];
        if reverse { input.reverse(); }
        for row in input { add(&mut rows, &mut sess, "Pi", "m", "2026-01-01", row); }
        let row = rows.values().next().unwrap();
        assert!(row.cost_known && row.cost_partial);
        assert_eq!(row.cost, 1.0);
        let mut combined = Rows::new();
        add(&mut combined, &mut sess, "Pi", "m", "2026-01-01", row.clone());
        assert!(combined.values().next().unwrap().cost_partial);
    }
    let (mut rows, mut sess) = (Rows::new(), Sess::new());
    add(&mut rows, &mut sess, "Pi", "m", "2026-01-01", Row { cost_known: true, input: 100, ..Default::default() });
    add(&mut rows, &mut sess, "Pi", "m", "2026-01-01", Row { tools: 1, ..Default::default() });
    assert!(!rows.values().next().unwrap().cost_partial);
}

#[test]
fn unpriced_models_are_reported_even_if_an_earlier_day_was_priced() {
    let home = Home::new();
    home.lines(".pi/agent/sessions/pi.jsonl", &[
        json!({"type": "message", "timestamp": "2026-01-01T00:00:00Z", "message": {
            "role": "assistant", "model": "audit-unknown", "usage": {"input": 100, "cost": {"total": 1}}
        }}),
        json!({"type": "message", "timestamp": "2026-01-02T00:00:00Z", "message": {
            "role": "assistant", "model": "audit-unknown", "usage": {"input": 100}
        }})
    ]);
    assert_eq!(scan_home(&home.0, &|_| {}).unpriced, vec!["audit-unknown"]);
}

#[test]
fn watchers_discover_new_sources_and_instances_and_retry_failed_registrations() {
    let home = Home::new();
    let mut watched = HashSet::new();
    assert!(!register_log_watches(&home.0, &mut watched, |_, _| true));
    let pi = home.0.join(".pi/agent/sessions");
    fs::create_dir_all(&pi).unwrap();
    assert!(!register_log_watches(&home.0, &mut watched, |_, _| false));
    assert!(register_log_watches(&home.0, &mut watched, |_, _| true));
    assert!(watched.contains(&pi));
    assert!(!register_log_watches(&home.0, &mut watched, |_, _| panic!("already registered")));
    for id in ["one", "two"] {
        let path = home.0.join(format!(".t3/userdata/providers/antigravity/{id}/antigravity-acp"));
        fs::create_dir_all(&path).unwrap();
        assert!(register_log_watches(&home.0, &mut watched, |_, _| true));
        assert!(watched.contains(&path));
    }
    fs::remove_dir_all(&pi).unwrap();
    register_log_watches(&home.0, &mut watched, |_, _| true);
    assert!(!watched.contains(&pi));
    fs::create_dir_all(&pi).unwrap();
    assert!(register_log_watches(&home.0, &mut watched, |_, _| true));
}

#[test]
fn empty_home_is_a_valid_scan() {
    let home = Home::new();
    let stats = scan_home(&home.0, &|_| {});
    assert!(stats.rows.is_empty());
    assert!(stats.sessions.is_empty());
    assert!(stats.limits.is_empty());
    assert_eq!(stats.sources.len(), 7);
    assert!(stats.sources.iter().all(|s| !s.found));
    assert!(stats.pricing.embedded > 0);
}
