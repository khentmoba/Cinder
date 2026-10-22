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

Each source is read on its own: one agent whose logs trip the parser up does
not take the rest of the dashboard with it. The **Data sources** panel says
which source failed and why, so a gap in the numbers is explained rather than
just missing.

### OpenCode, precisely

OpenCode writes its usage into a SQLite database that is kept in write-ahead
mode, and it has changed shape: the current version keeps the messages in
`session_message`, with the model and the content inside the message, while
the older `message` table is left behind holding a copy of the older ones.
Only the current table is read — the old one holds the same messages, so
reading both would count them twice. The stamp of the write-ahead log beside
the database is part of the cache key, so a message written a moment ago
counts even before it is checkpointed into the file.

Two things are worth knowing about the numbers:

- a **compaction** is a model call of its own, and the tokens it cost are
  counted, even though the messages it replaced may be gone;
- the tokens of messages OpenCode has **deleted** — a branch or a revert
  takes them away — are counted by OpenCode's own session totals but cannot
  be read from the log, because no message is left to read them from. They
  are about 0.4% of the input tokens on a machine with a long history, and
  no reader can do better.

`scripts/check-opencode-logs.mjs` reads the same database with an
implementation that shares no code with the collector, and the two have to
agree line for line.

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

## Alerts

Cinder watches the live windows and says something when they are about to run
out. A window is projected forward from how fast it is being consumed, so a
window that is 40% used a tenth of the way through will not last:

- **critical** — the window is nearly spent, or it will run out before it
  resets.
- **warn** — the window is on pace to use most of it.

The alert appears at the top of the Limits view, and the Limits button carries
a count so a problem is visible without opening the view. The first time an
alert shows up it also arrives as a desktop notification; the same alert is
not announced twice, and one that clears is forgotten so it can fire again
later.

## Plans and budgets

A **plan** is a flat rate you pay each month (Claude Pro, ChatGPT Plus) and a
**budget** is a limit on what you may spend. They are not the same thing, and
the dashboard keeps them apart:

- A **budget** burns down: "$12.40 of your $50.00 budget (25%)", and crossing
  80% raises an alert.
- A **plan** does not. Cinder prices every token at list prices, so the
  figure it shows against a plan is the *value of the work delivered*, not an
  amount you pay. It reads "Claude Pro · $20.00 a month · the work was worth
  $142.50 (7.1x the plan)". The limit on a plan is the provider's quota
  window, not a dollar amount, so a plan never raises a budget alert.

Declare them in **Settings → Plans**. They are stored in `~/.cinder/plans.json`
and are read against the current month.

## System tray

Cinder puts an icon in the tray whose tooltip carries the live quota
percentages, so the window that matters can be read without opening the app.
**Clicking the icon opens a popover** next to it with the whole picture:

- one card per provider that is signed in on this machine, with how much of
  each quota window is gone, when it resets, and the plan it is on;
- the alerts that are worth interrupting the user about;
- the plans the user declared, and what the work was worth against them;
- what today cost, in tokens and in money.

Right-click opens **Open Cinder**, **Rescan now** and **Quit Cinder**. The
popover closes when it loses the focus, the way a menu does, and the refresh
button in its corner asks the providers again.

A provider that cannot be read says so in its card rather than showing a
number that is not true. A provider whose newest refresh failed keeps the
last numbers that did come back, badged **stale**, so a flaky network never
blanks the panel.

> `CINDER_TRAY_POPOVER=1 cinder` opens the popover at start-up, which is how
> it is checked without a mouse. `CINDER_DEBUG=1` prints what the tray is
> doing to the console. `scripts/screenshot.ps1` and `scripts/tray-click.ps1`
> take a picture of the screen and click the tray icon through UI Automation,
> so the popover can be looked at on a machine with no hands on the mouse.

## Live quotas, and keeping them true

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
| OpenCode | `~/.local/share/opencode/auth.json` | `opencode.ai/zen/go/v1/usage` |
| Kimi Code | `~/.kimi/credentials/*.json` | `api.kimi.com/coding/v1/usages` |
| OpenRouter | `OPENROUTER_API_KEY` or the agent catalogs | `openrouter.ai/api/v1/credits` |
| DeepSeek | `DEEPSEEK_API_KEY` or the agent catalogs | `api.deepseek.com/user/balance` |

Each endpoint was taken from the provider's own code or its published API,
and each was checked to answer 401 rather than 404 — the route is there and
it wants a bearer token. A provider Cinder cannot find a key for gets no card:
an agent that is not used here is not a problem.

### Adding a provider of your own

Not every service worth watching answers with a shape Cinder knows. A provider
of your own is a file, `~/.cinder/providers.json`:

```json
{
  "providers": [
    {
      "name": "My Router",
      "url": "https://api.example.com/usage",
      "token": "sk-...",
      "percent": "data.usage.percent",
      "reset": "data.usage.reset_at",
      "detail": "data.usage.label"
    }
  ]
}
```

`percent` is a dotted path to the number that is the percentage used, or to an
object with `used` and `limit` in it. `reset` and `detail` are optional. The
token is sent as `Authorization: Bearer <token>`, so only point it at a
service you trust with it.

A provider that is not signed in is skipped; one that is signed in but fails
says so in the panel instead of showing a stale number. A window whose reset
time has already passed is dropped rather than shown as if it were still
running.

When a provider explains itself, the card quotes it: an expired key says
*"This API key has expired. Create a new API key in OpenCode Console and
replace it in your client"*, which is more use than "HTTP 401".

Anthropic sends the list of the windows a plan has, and Cinder reads it: it
names windows the named fields do not (a weekly Opus limit, say), and it
carries the reset time for the ones that leave it out.

The list also carries a flag that says whether a window is active. Cinder does
not read it: on a real account a session window sits at 22% with a reset time
four hours away and is still flagged inactive, so the flag does not mean what
it looks like. The percentage and the reset time are the truth.

The fetcher is built for a bad day, not only for a good one:

- a request that fails is **tried again** with a backoff — and a rate limit
  waits longer, because it means "not yet";
- a provider that keeps failing is **benched** for a while (a minute, then
  two, then five, then a quarter of an hour) instead of being asked every
  few minutes;
- the last windows that did come back are **kept and marked stale**, so a
  flaky network never blanks the panel;
- the snapshot is **written to disk**, so a launch shows the last known state
  instead of an empty tray while the first refresh is in flight;
- a token that is about to expire is **refreshed** before it is used, and the
  new token is written back, which also keeps the agent signed in;
- a machine with two Claude or Codex logins gets a **card for each** of them.

## Prices

Every token is priced against the public list prices, so the dollar figure on
screen is an *equivalent value*, never an invoice. The bundled price list is
a snapshot; a fresh one is fetched once a day in the background and kept in
`~/.cinder/prices.json`, so a model that appeared last week is priced at this
week's price instead of showing up as unpriced. The bundled list is still
there underneath, so a fetch that fails changes nothing.

When a model has no price at all, but the agent itself wrote down what the
call cost, that figure is used instead: the number the agent knows is better
than no number. Those rows are marked as **reported** rather than estimated.

The agent catalogues on this machine (Pi's model store, the Cline pass price
list) know what each route charges, and that price is used when the route is
known. When they disagree about a bare model name - a subscription route and a
pay-as-you-go route pricing the same model - the route that served the
request is not known, so the public list price is used instead. The work was
not free, and a list price is the same figure the rest of the dashboard
quotes.

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
- **Limits**: a *view* (`Usage | Limits`), not a metric. Features burn-rate pacing (`⚡ %/hr`), visual quota health badges (`On Pace`, `Elevated`, `Exhaustion Risk`), linear pace markers on the progress track, and depletion ETA forecasts. Live windows are badged **live**, and the alerts and declared plans sit above them.
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
`-- --ignored live_quota_check` asks the providers for their live quota, and
`-- --ignored live_price_refresh` fetches the real price list. Both reach the
network.

### Checking the numbers against the logs

The collector and the raw logs are checked against each other, so a number on
the screen is a number that can be accounted for:

```bash
powershell -File scripts/check-logs.ps1                              # Claude Code
powershell -File scripts/check-logs.ps1 -Agent Codex                 # Codex
powershell -File scripts/check-logs.ps1 -Agent Pi                    # Pi
powershell -File scripts/check-logs.ps1 -Agent OpenCode              # OpenCode
powershell -File scripts/check-logs.ps1 -Agent DSH                   # DSH
powershell -File scripts/check-logs.ps1 -Agent Antigravity           # T3's Antigravity
powershell -File scripts/check-logs.ps1 -Agent Codex 2026-10-19 2026-10-20
```

`scripts/check-claude-logs.mjs`, `scripts/check-codex-logs.mjs`,
`scripts/check-pi-logs.mjs`, `scripts/check-opencode-logs.mjs`,
`scripts/check-dsh-logs.mjs` and `scripts/check-t3-logs.mjs` read the agents'
own logs with an implementation that shares no code with the collector, and
the `real_home_day_check` test prints what the collector made of the same
files. The two have to agree line for line, per model, on requests and on
every kind of token:

```
=== 2026-10-21 ===
  OK: 2 models, the collector and the raw logs agree
    ROW	gpt-6.1-sol	972	4203575	151414400	0	452778
    ROW	gpt-6-luna	957	5115851	123688064	0	986159
```

The Codex check is the interesting one: it pins down the split between cached
and uncached input, the model switching in the middle of a file, and the
forked rollouts that replay their parent's earlier turns. The DSH one has to
walk the chain of zstd frames the sessions are written in, and the Antigravity
one reads the same usage out of two places — T3's own orchestration database
and the IDE transcripts — where the model is named in a line of prose.

### Checking the numbers against the agent's own record

Claude Code writes down what it charged and the usage that made up the charge,
in a `cost-state` line per session. That is the agent's own record of the
same turns the collector reads from the transcript, so the two can be
compared:

```bash
node scripts/check-claude-sessions.mjs          # session by session
node scripts/check-claude-sessions.mjs --no-dedup   # what counting every record gives
node scripts/check-claude-price.mjs             # the price list against what it charged
node scripts/check-claude-own-totals.mjs 2026-10-22
```

This is how the biggest counting bug was found. Claude Code writes one record
per content block of a turn - the thinking block, then one for each tool call
- and every one of those records carries the whole turn's usage. Counting the
records as they come counts a turn once per tool call it made, and the
transcript then holds about twice the usage the CLI recorded:

```
sessions with a cost-state: 34
  the transcript and the CLI's total agree exactly: 7
  median ratio transcript/CLI: 0.999      # one record per message id
  median ratio transcript/CLI: 2.067      # every record counted
```

The API message id is the same in all of a turn's records, so the usage is
counted once per id. The tool calls are still counted from every record,
because each block is written exactly once.

The same file answers a second question: what the CLI charged against the
price list. Five of the six models it used agree with the list price to a
tenth of a percent:

```
claude-opus-5-5              charged $105.7749  list price $105.7913  diff +0.0%
claude-sonnet-5-5            charged $20.4019   list price $20.4091   diff +0.0%
claude-haiku-4-5             charged $0.0260    list price $0.0260    diff +0.0%
claude-opus-5-5[1m]          charged $11.1364   list price $11.1364   diff +0.0%
claude-haiku-5-5             charged $17.5566   list price $4.7297    diff -73.1%
```

The last one is a known gap: Anthropic prices that model with a premium past
a hundred thousand tokens, and Cinder estimates with one flat rate, so the
figure is too low for it. The premium is not modelled yet, and the check says
so rather than hiding it.

Two price bugs came out of the same comparison. A 1-hour cache write is
billed at twice the *input* price, not twice the 5-minute write price, which
is 25% more than it should be - the upstream catalogue carries the rate as
`cache_creation_input_token_cost_above_1hr`. And when the agent catalogues on
this machine disagree about a model's price, the public list price is used
instead of nothing at all: Claude Code's opus row went from $0.00 a day to
$30.94, and the day's figure from a partial `$10.42` to a complete one.

### Checking the popover against the provider's own numbers

The numbers on the popover come from the provider's own quota API, and they
are checked against the provider's own record of them:

```bash
node scripts/check-live-quota.mjs
```

The Codex CLI writes its copy of the limits into its own session log, so
there are three numbers to compare: what the CLI wrote down, what the usage
API answers now, and what Cinder reads out of that API. The first two are
independent of each other — the CLI writes the server's answer into the log,
and the second is a fresh fetch — so when they agree, the number on the
popover is the provider's number:

```
the CLI wrote its limits at 2026-10-21T21:45:47.547Z
  5-hour window  log  70%  api  70%  gap 0  reset the same
  7-day window   log  89%  api  89%  gap 0  reset the same
  the popover shows the provider's own numbers
```

The reset times have to match exactly; the percentages may have moved a
little between the log being written and the fetch, so they are compared
with a small tolerance.

Anthropic sends the list of the windows a plan has, and Cinder reads it for
the windows the named fields do not carry and for the reset times they leave
out. The flag in that list that says whether a window is active is left
alone: on a real account a session window sits at 22% with a reset time four
hours away and is still flagged inactive.

Add agents by extending `parse_*` functions in `src-tauri/src/main.rs` and
`src-tauri/src/agents.rs`.
