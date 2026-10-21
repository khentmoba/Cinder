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
| Cursor | `~/.cursor/chats/**` + `~/.cursor/cli-chats/**` | requests, tokens, models |
| GitHub Copilot | `~/.copilot/history-session-state/**` + `~/.copilot/logs/**` | requests, tokens, models |
| Gemini CLI | `~/.gemini/tmp/**` + `~/.gemini/sessions/**` | requests, tokens, models |
| Cline | VS Code `globalStorage/saoudrizwan.claude-dev/**` | requests, tokens, models |

Agents that log no token or cost field (older Antigravity IDE transcripts) are
still fully visible: use the **Requests** metric. Cost/tokens views show a `—`
for them by design, so the agent list also prints the request count next to
the cost.

Daily ranges use UTC dates; **Today** is a UTC calendar day, not a rolling
24-hour window. Costs marked **≥** exclude some unpriced token usage. Codex
cache counts are kept as reported, without borrowing ratios from other agents.

## What the numbers mean

Cinder prices every token against public list prices, so the dollar figure is
an **equivalent value**, never an invoice. Each row carries the billing mode
it was collected under, and the dashboard says which one it is looking at:

- **Plan** — the route is covered by a flat-rate plan (Claude Pro/Max, ChatGPT,
  Copilot, the Antigravity plan). The card is not charged per token.
- **API** — billed per token through a provider key.
- **Local** — local inference, so no provider bill at all. Local runs also show
  what the same tokens would have cost on a cloud model.

The mode is read from the agent's own credential file where there is one:
`~/.claude/.credentials.json` says whether Claude Code runs on a subscription or
an API key, and `~/.codex/auth.json` says the same for Codex. Where there is
no such evidence, the row is left unlabelled rather than guessed.

## Projects, worktrees and subagents

A run is attributed to the repository it happened in. A git worktree is
resolved back to its main checkout (through the `gitdir:` pointer in its
`.git` file), so every branch of a repository counts as one project instead of
several. A checkout that has since been cleaned up still rolls up through the
`worktrees` folder it lived in.

A delegated child run (Claude Code writes it to `<session>/subagents/`) is
counted once, on its own row, badged as a subagent and tagged with the model
it actually ran on — which is often a cheaper one than its parent.

## Live quotas

The **Limits** view shows two kinds of window: the ones an agent wrote into
its own session files, and the ones fetched live from the provider. The live
ones use the OAuth token the agent already stored on disk, and are refreshed
every five minutes in the background, so a scan never waits on the network.

| Agent | Where the credential is | Endpoint |
|---|---|---|
| Claude Code | `~/.claude/.credentials.json` | `api.anthropic.com/api/oauth/usage` |
| Codex | `~/.codex/auth.json` | `chatgpt.com/backend-api/wham/usage` |
| GitHub Copilot | `~/.config/github-copilot/apps.json` | `api.github.com/copilot_internal/user` |
| Gemini CLI | `~/.gemini/oauth_creds.json` | `cloudcode-pa.googleapis.com` |
| OpenCode | `OPENCODE_API_KEY` or `~/.local/share/opencode/auth.json` | `opencode.ai/api/usage` |

A provider that is not signed in is skipped; one that is signed in but fails
says so in the panel instead of showing a stale number.

## System tray

Cinder puts an icon in the tray whose tooltip carries the live quota
percentages, so the window that matters can be read without opening the app.
Left-click brings the dashboard back, right-click opens **Open Cinder**,
**Rescan now** and **Quit Cinder**.

## Speed

The session logs on a busy machine run to gigabytes, and a scan has to find
the few lines in them that can change a number. Two things make that fast:

- Every line is screened for a handful of marker strings with
  [FFF](https://github.com/dmtrKovalenko/fff)'s SIMD substring search before
  it is ever handed to `serde_json`. It picks the two rarest bytes of the
  needle and compares them in parallel, which is about 1.4x faster than
  `str::contains` over the same lines.
- The OpenCode database is aggregated inside SQLite rather than by pulling
  every message's JSON into Rust, which took the source from 19 s to under 2 s.

A warm scan (nothing changed since the last one) of a machine with 1.7 GB of
logs now takes about 2 s, and the **Data sources** panel shows how long each
source took.

## Using the dashboard

- **Filters stack**: click an agent row, then a model row, to narrow every panel
  at once. The breadcrumb has one click per segment to undo it.
- **Chart**: click a legend chip to hide an agent. Ranges over six weeks bucket
  into weeks, and the dashed line is the previous period of the same length.
- **Breakdown**: switch between **Model**, **Day**, **Agent**, **Cache** and
  **Project** views; rows are clickable filters, **Show all** lists every row,
  and **CSV** exports the tailored dataset.
- **Limits**: a *view* (`Usage | Limits`), not a metric. Features burn-rate pacing (`⚡ %/hr`), visual quota health badges (`On Pace`, `Elevated`, `Exhaustion Risk`), linear pace markers on the progress track, and depletion ETA forecasts. Live windows are badged **live**.
- **Artifacts**: the links the agents published during the scanned period.
- **Keys**: `1`–`4` cost / tokens / requests / limits, `R` refresh, `S` settings,
  `Esc` closes a modal. Window size and position are remembered between launches.

## Tests

```bash
npm test                          # dashboard regressions (Node.js 18+)
cargo test --manifest-path src-tauri/Cargo.toml  # synthetic logs and databases
```

Tests use temporary home directories, not your real agent logs.

`cargo test --manifest-path src-tauri/Cargo.toml -- --ignored real_home_scan`
scans the machine you are on and prints what it found, for a human to eyeball.

Add agents by extending `parse_*` functions in `src-tauri/src/main.rs` and
`src-tauri/src/agents.rs`.
