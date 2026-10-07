# Cinder

Desktop app (Tauri 2) showing your coding-agent usage stats on this machine.

## Run

```bash
npm install
npm run dev        # dev window
npm run build      # installers in src-tauri/target/release/bundle
npm run install-local  # build + silently reinstall, so the desktop
                       # shortcut picks up the change on next open
```

> The desktop shortcut launches `%LocalAppData%\Cinder\cinder.exe`, not the
> repo. After any code change, run `npm run install-local` (or re-run the
> NSIS setup by hand) — otherwise the shortcut keeps opening the stale build.

## Live updates

Cinder watches the log stores below with OS file notifications and pushes fresh
stats into the open window (`stats-updated` event): debounced 10 s after writes
go quiet, at most one rescan per minute. No refresh clicks, no polling reads
while idle. Manual refresh via the ↻ button still works and serialises with the
watcher through a shared scan lock.

## Data sources (parsed locally, no network)

| Agent | Location | Metrics |
|---|---|---|
| Pi | `~/.pi/agent/sessions/**/*.jsonl` | requests, tokens, cost, tool calls, models |
| Codex | `~/.codex/sessions/**/*.jsonl` | requests, tokens, tools, models; cost estimated |
| Claude Code | `~/.claude/projects/**/*.jsonl` | requests, tokens, tools, models; cost estimated |
| OpenCode | `~/.local/share/opencode/opencode.db` | sessions, tokens, cost, tool calls |
| Antigravity | `~/.gemini/antigravity*/` + T3 `~/.t3/userdata/statev2.sqlite` and `~/.t3/userdata/providers/antigravity/*/antigravity-acp` | requests, tools, sessions, T3-reported tokens; API cost estimated |
| DSH | `~/.dsh/sessions/**/session.v4.jsonl.zstd` | requests, tokens, tools, sessions, models; cost estimated |
| LM Studio | `~/.lmstudio/conversations/*.conversation.json` | requests, sessions, models (no token/cost) |

Agents that log no token or cost field (older Antigravity IDE transcripts) are
still fully visible: use the **Requests** metric. Cost/tokens views show a `—`
for them by design, so the agent list also prints the request count next to
the cost.

Add agents by extending `parse_*` functions in `src-tauri/src/main.rs`.
