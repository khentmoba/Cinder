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

## Settings & in-app updates

Click **⚙ Settings** in the header or the version button in the footer at any time to open the Settings modal:
- **Updates**: Check for updates on demand via GitHub Releases (`khentmoba/Cinder/releases`), inspect release notes, and install updates directly.
- **In-place updates**: Installing an update performs an actual in-place update (overwrites updated binaries and assets seamlessly without uninstalling previous versions first).
- **Preferences**: Customize your preferred default metric (Cost, Tokens, Requests, Limits) and default range (Today, 7 days, 30 days, 90 days) on startup.
- **Data sources**: Inspect all local log paths and SQLite stores monitored by Cinder, or trigger an immediate full rescan.
- **Automatic detection**: When a newer release is published on GitHub, a prominent update badge (`Update vX.Y.Z`) appears in the header and an indicator lights up on the Settings button.
- **Native installation**: Clicking **Update Now** downloads the release installer (`.exe`) directly in-app with real-time download progress, runs the silent NSIS installer in update mode (`/UPDATE`), and automatically restarts Cinder on the new version.

### Publishing a release

1. Update version in `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.
2. Tag and push to GitHub:
   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```
3. The GitHub Actions Release workflow (`.github/workflows/release.yml`) builds the Windows NSIS installer, creates the GitHub Release, and attaches the setup binary. Existing installations will immediately detect the update.

## Live updates

Cinder watches the log stores below with OS file notifications and pushes fresh
stats into the open window (`stats-updated` event): debounced 10 s after writes
go quiet, at most one rescan per minute. Source directories are checked every
5 s to discover newly installed agents and T3 instances; log contents are read
only for scans, not polled while idle. Manual refresh via the ↻ button still
works and serialises with the watcher through a shared scan lock.

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

Daily ranges use UTC dates; **Today** is a UTC calendar day, not a rolling
24-hour window. Costs marked **≥** exclude some unpriced token usage. Codex
cache counts are kept as reported, without borrowing ratios from other agents.

## Using the dashboard

- **Filters stack**: click an agent row, then a model row, to narrow every panel
  at once. The breadcrumb has one click per segment to undo it.
- **Chart**: click a legend chip to hide an agent. Ranges over six weeks bucket
  into weeks, and the dashed line is the previous period of the same length.
- **Breakdown**: rows are clickable filters; **Show all** lists every row and
  **CSV** exports the visible table.
- **Limits**: a *view* (`Usage | Limits`), not a metric. Each card shows how much
  of the window is gone, when it resets, and — once the window is under way —
  where the current burn rate lands by reset.
- **Keys**: `1`–`4` cost / tokens / requests / limits, `R` refresh, `S` settings,
  `Esc` closes a modal. Window size and position are remembered between launches.

## Tests

```bash
npm test                          # dashboard regressions (Node.js 18+)
cargo test --manifest-path src-tauri/Cargo.toml  # synthetic logs and databases
```

Tests use temporary home directories, not your real agent logs.

Add agents by extending `parse_*` functions in `src-tauri/src/main.rs`.
