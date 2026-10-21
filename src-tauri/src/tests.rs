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
fn cache_preserves_rows_and_sessions_and_detects_modification() {
    let home = Home::new();
    home.lines(".pi/agent/sessions/s1.jsonl", &[
        json!({"type": "session", "timestamp": "2026-01-01T00:00:00Z"}),
        json!({"type": "message", "timestamp": "2026-01-01T00:01:00Z", "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 100, "output": 50}
        }})
    ]);

    // Initial scan populates cache
    let stats1 = scan_home(&home.0, &|_| {});
    let r1 = stats1.rows.iter().find(|r| r.agent == "Pi").unwrap();
    assert_eq!(r1.input, 100);
    assert_eq!(r1.output, 50);

    // Warm scan hits cache
    let stats2 = scan_home(&home.0, &|_| {});
    let r2 = stats2.rows.iter().find(|r| r.agent == "Pi").unwrap();
    assert_eq!(r2.input, 100);
    assert_eq!(r2.output, 50);

    // File is updated
    home.lines(".pi/agent/sessions/s1.jsonl", &[
        json!({"type": "session", "timestamp": "2026-01-01T00:00:00Z"}),
        json!({"type": "message", "timestamp": "2026-01-01T00:01:00Z", "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 200, "output": 100}
        }})
    ]);

    // Warm scan detects modification
    let stats3 = scan_home(&home.0, &|_| {});
    let r3 = stats3.rows.iter().find(|r| r.agent == "Pi").unwrap();
    assert_eq!(r3.input, 200);
    assert_eq!(r3.output, 100);
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
    assert_eq!(stats.sources.len(), 13);
    assert!(stats.sources.iter().all(|s| !s.found));
    assert!(stats.pricing.embedded > 0);
    // Every source is named and located, so the panel can list them all.
    assert!(stats.sources.iter().all(|s| !s.agent.is_empty() && !s.path.is_empty()));
    let mut names: Vec<&str> = stats.sources.iter().map(|s| s.agent.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), stats.sources.len());
}

#[test]
fn the_billing_mode_comes_from_the_agent_credential_files() {
    let home = Home::new();
    // Nothing signed in: the mode is unknown rather than a guess.
    assert_eq!(claude_route(&home.0), Route::Unknown);
    assert_eq!(codex_route(&home.0), Route::Unknown);

    // A Claude subscription.
    home.write(
        ".claude/.credentials.json",
        r#"{"claudeAiOauth":{"accessToken":"x","subscriptionType":"pro"}}"#,
    );
    assert_eq!(claude_route(&home.0), Route::Plan);

    // A ChatGPT plan for Codex.
    home.write(
        ".codex/auth.json",
        r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"access_token":"x"}}"#,
    );
    assert_eq!(codex_route(&home.0), Route::Plan);

    // An API key bills per token.
    home.write(".codex/auth.json", r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-x"}"#);
    assert_eq!(codex_route(&home.0), Route::Api);
}

#[test]
fn a_reported_cost_says_how_a_row_is_billed() {
    // A real charge is per token.
    assert_eq!(billing_of(Some(1.5), true, false), Billing::Api);
    // A reported zero with no public price means the route is on a plan.
    assert_eq!(billing_of(Some(0.0), false, false), Billing::Subscription);
    // Our own estimate of an API-billed run.
    assert_eq!(billing_of(None, true, false), Billing::Api);
    // Nothing to go on.
    assert_eq!(billing_of(None, false, false), Billing::Unknown);
    // A local model costs nobody anything.
    assert_eq!(billing_of(None, false, true), Billing::Local);
}

#[test]
fn a_claude_subagent_run_is_tagged_and_counted_once() {
    let home = Home::new();
    let usage = json!({
        "type": "assistant", "cwd": "C:/repo", "timestamp": "2026-01-01T00:00:00Z",
        "message": {"role": "assistant", "model": "claude-haiku-4-5",
            "usage": {"input_tokens": 10, "output_tokens": 5}}
    });
    // The parent session.
    home.lines(".claude/projects/-C--repo/parent.jsonl", std::slice::from_ref(&usage));
    // The delegated child run, in its own file.
    home.lines(
        ".claude/projects/-C--repo/parent/subagents/agent-1.jsonl",
        &[json!({"type": "user", "isSidechain": true, "timestamp": "2026-01-01T00:01:00Z"}), usage],
    );

    let stats = scan_home(&home.0, &|_| {});
    let rows: Vec<&Row> = stats.rows.iter().filter(|r| r.agent == "Claude Code").collect();
    // Both runs are counted, and only the child is marked as delegated.
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r.subagent).count(), 1);
    assert_eq!(rows.iter().map(|r| r.requests).sum::<u64>(), 2);
    // Both belong to the same project.
    assert!(rows.iter().all(|r| r.project == "repo"));
}

#[test]
fn a_1h_cache_write_is_priced_at_its_own_rate() {
    let home = Home::new();
    home.lines(".claude/projects/-C--repo/s.jsonl", &[json!({
        "type": "assistant", "cwd": "C:/repo", "timestamp": "2026-01-01T00:00:00Z",
        "message": {"role": "assistant", "model": "claude-sonnet-4-5",
            "usage": {"input_tokens": 0, "output_tokens": 0,
                "cache_creation_input_tokens": 1000,
                "cache_creation": {"ephemeral_1h_input_tokens": 1000},
                "cache_read_input_tokens": 0}}
    })]);

    let stats = scan_home(&home.0, &|_| {});
    let r = stats.rows.iter().find(|r| r.agent == "Claude Code").unwrap();
    assert_eq!(r.cache_write, 1000);
    assert_eq!(r.cache_write_1h, 1000);
    assert!(r.cost_known);
    // The 1-hour write costs twice the 5-minute rate, and the 5-minute rate
    // is 1.25x the base input price when the catalog has no cache-write
    // price of its own.
    let expected = 1000.0 * (3.0 * 1.25 * 2.0) / 1e6;
    assert!((r.cost - expected).abs() < 1e-9, "cost {} expected {expected}", r.cost);
}

#[test]
fn a_published_artifact_is_collected_once() {
    let home = Home::new();
    let turn = json!({
        "type": "assistant", "cwd": "C:/repo", "timestamp": "2026-01-01T00:00:00Z",
        "message": {"role": "assistant", "model": "claude-sonnet-4-5",
            "content": [{"type": "tool_use", "name": "Artifact",
                "input": {"id": "abc123", "title": "A chart"}}],
            "usage": {"input_tokens": 10, "output_tokens": 5}}
    });
    home.lines(".claude/projects/-C--repo/s.jsonl", &[turn.clone(), turn]);

    let stats = scan_home(&home.0, &|_| {});
    assert_eq!(stats.artifacts.len(), 1);
    assert_eq!(stats.artifacts[0].url, "https://claude.ai/code/artifact/abc123");
    assert_eq!(stats.artifacts[0].title, "A chart");
    assert_eq!(stats.artifacts[0].day, "2026-01-01");
}

#[test]
fn a_worktree_rolls_up_into_its_repository() {
    let home = Home::new();
    let repo = home.0.join("mainrepo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let worktree = home.0.join("worktrees").join("mainrepo").join("feature");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", repo.join(".git").join("worktrees").join("feature").to_string_lossy()),
    )
    .unwrap();
    let cwd = worktree.to_string_lossy().to_string();
    home.lines(".pi/agent/sessions/s.jsonl", &[
        json!({"type": "session", "timestamp": "2026-01-01T00:00:00Z", "cwd": cwd}),
        json!({"type": "message", "timestamp": "2026-01-01T00:01:00Z", "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 10, "output": 5}
        }})
    ]);

    let stats = scan_home(&home.0, &|_| {});
    let r = stats.rows.iter().find(|r| r.agent == "Pi").unwrap();
    assert_eq!(r.project, "mainrepo");
}

#[test]
fn a_scan_reports_the_alerts_the_current_numbers_justify() {
    let home = Home::new();
    let now = now_secs();
    // A window that is 40% used with a fifth of its time gone: on pace for
    // 200%, so it will run out before it resets.
    let reports = vec![quota::QuotaReport::ok("Claude Code", vec![quota::LiveQuota {
        agent: "Claude Code".into(),
        window: "5h".into(),
        used_percent: 40.0,
        window_minutes: 300,
        resets_at: now + 300 * 60 * 4 / 5,
        plan: "pro".into(),
        detail: "5-hour session window".into(),
        ..Default::default()
    }])];
    let stats = scan_home_with(&home.0, &|_| {}, reports, "2026-10-21T00:00:00Z".into());
    assert_eq!(stats.alerts.len(), 1);
    assert_eq!(stats.alerts[0].severity, alerts::Severity::Critical);
    assert!(stats.alerts[0].title.contains("run out"));
    // The window is in the limits list as a live one.
    assert!(stats.limits.iter().any(|l| l.source == "live" && l.window == "5h"));
}

#[test]
fn a_budget_over_its_limit_raises_an_alert() {
    let home = Home::new();
    // The plan is read against the current month, so the usage has to fall in
    // it for the budget to mean anything.
    let today = unix_day(now_secs());
    budget::save(&home.0, &[Plan {
        agent: "Pi".into(),
        kind: budget::PlanKind::Budget,
        limit: 0.001,
        label: "monthly".into(),
    }])
    .unwrap();
    home.lines(".pi/agent/sessions/s.jsonl", &[
        json!({"type": "session", "timestamp": format!("{today}T00:00:00Z")}),
        json!({"type": "message", "timestamp": format!("{today}T00:01:00Z"), "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 1000, "output": 100}
        }})
    ]);
    let stats = scan_home_with(&home.0, &|_| {}, Vec::new(), String::new());
    assert_eq!(stats.plans.len(), 1);
    assert!(stats.plans[0].percent > 100.0, "percent {}", stats.plans[0].percent);
    assert!(stats.alerts.iter().any(|a| a.title.contains("over its budget")));
    // The plan is measured against the spend the rows actually report.
    let spend: f64 = stats.rows.iter().filter(|r| r.agent == "Pi").map(|r| r.cost).sum();
    assert_eq!(stats.plans[0].used, spend);
}

#[test]
fn a_plan_is_reported_as_the_value_the_work_was_worth() {
    let home = Home::new();
    let today = unix_day(now_secs());
    budget::save(&home.0, &[Plan {
        agent: "Pi".into(),
        kind: budget::PlanKind::Subscription,
        limit: 0.001,
        label: "Pi Pro".into(),
    }])
    .unwrap();
    home.lines(".pi/agent/sessions/s.jsonl", &[
        json!({"type": "session", "timestamp": format!("{today}T00:00:00Z")}),
        json!({"type": "message", "timestamp": format!("{today}T00:01:00Z"), "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 1000, "output": 100}
        }})
    ]);
    let stats = scan_home_with(&home.0, &|_| {}, Vec::new(), String::new());
    // A plan is not a budget: being over the plan price is not an alert.
    assert_eq!(stats.plans.len(), 1);
    assert!(stats.plans[0].multiple > 1.0);
    assert!(stats.alerts.is_empty(), "alerts {:?}", stats.alerts);
}

/// Asks the providers for their live quota, on the machine this runs on.
///
/// Ignored by default because it reaches the network:
/// `cargo test --bin cinder -- --ignored --nocapture live_quota_check`.
#[test]
#[ignore]
fn live_quota_check() {
    let home = dirs::home_dir().unwrap();
    let reports = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(quota::fetch_all(&home));
    for r in &reports {
        if r.skipped {
            println!("  {:<18} not signed in", r.agent);
            continue;
        }
        if !r.error.is_empty() {
            println!("  {:<18} ERROR {}", r.agent, r.error);
            continue;
        }
        for w in &r.windows {
            println!(
                "  {:<18} {:<12} {:>5.1}%  resets in {:>6}  {}",
                w.agent,
                w.window,
                w.used_percent,
                w.resets_at - now_secs(),
                w.detail
            );
        }
    }
    // The whole chain: the live windows become the alerts the panel shows.
    let sandbox = Home::new();
    let today = unix_day(now_secs());
    sandbox.lines(".pi/agent/sessions/s.jsonl", &[
        json!({"type": "session", "timestamp": format!("{today}T00:00:00Z")}),
        json!({"type": "message", "timestamp": format!("{today}T00:01:00Z"), "message": {
            "role": "assistant", "model": "gpt-5", "usage": {"input": 1000, "output": 100}
        }})
    ]);
    budget::save(&sandbox.0, &[Plan {
        agent: "Pi".into(),
        kind: budget::PlanKind::Budget,
        limit: 0.001,
        label: "monthly".into(),
    }])
    .unwrap();
    let stats = scan_home_with(&sandbox.0, &|_| {}, reports, "2026-10-21T00:00:00Z".into());
    println!("  alerts from the live windows:");
    for a in &stats.alerts {
        println!("    [{}] {} — {}", a.severity as u8, a.title, a.detail);
    }
    println!("  plans: {:?}", stats.plans);
}

/// A scan of the machine this test runs on, for a human to eyeball.
#[test]
#[ignore]
fn real_home_scan() {
    let home = dirs::home_dir().unwrap();
    let started = std::time::Instant::now();
    let stats = scan_home(&home, &|_| {});
    println!("scan took {:?}", started.elapsed());
    for s in &stats.sources {
        println!("  {:<18} {:>6} files  {:>7} ms  {}", s.agent, s.files, s.scan_ms, s.path);
    }
    let mut projects: std::collections::BTreeMap<String, u64> = Default::default();
    let mut billing: std::collections::BTreeMap<String, u64> = Default::default();
    let mut subagents = 0u64;
    for r in &stats.rows {
        *projects.entry(r.project.clone()).or_default() += r.requests;
        *billing.entry(format!("{:?}", r.billing)).or_default() += r.requests;
        if r.subagent {
            subagents += 1;
        }
    }
    println!("projects: {projects:?}");
    println!("billing: {billing:?}");
    println!("subagent rows: {subagents}");
    println!("limits: {:?}", stats.limits);
    println!("rows: {}", stats.rows.len());
    println!("alerts: {:?}", stats.alerts);
    println!("plans: {:?}", stats.plans);
    // How a git worktree resolves.
    for cwd in [
        r"C:\Users\Admin\.t3\worktrees\Cinder\t3-1e48cab7",
        r"C:\APPLICATIONS\Cinder",
        r"C:\Users\Admin\.t3\scratch\2026-10-03-can-you-check-and-see-447f1a0f",
    ] {
        println!("  {cwd} -> {:?}", project::canonical_repo(std::path::Path::new(cwd)));
    }
    for r in stats.rows.iter().filter(|r| r.project.starts_with("t3-")).take(6) {
        println!("  row agent={} model={} project={}", r.agent, r.model, r.project);
    }
    for r in stats.rows.iter().filter(|r| r.subagent).take(8) {
        println!("  subagent row agent={} model={} project={}", r.agent, r.model, r.project);
    }
}
