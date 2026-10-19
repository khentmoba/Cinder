---
name: create-pr
description: Create a pull request for Cinder following repo conventions and the pr-template structure. Runs test gates, generates visual summary, gathers before/after evidence, formats merge danger, and opens the PR via GitHub CLI.
---

# Create Pull Request

Creates a pull request for Cinder adhering to repo conventions and `pr-template`.

## 1. Local Pre-flight Verification

Run all required project test suites and linters before creating or updating a PR:

```bash
# 1. Frontend tests
npm test

# 2. Rust unit and integration tests
cargo test --manifest-path src-tauri/Cargo.toml

# 3. Rust clippy checks (strict - zero warnings)
cargo clippy --manifest-path src-tauri/Cargo.toml --tests -- -D warnings
```

All checks must pass cleanly.

## 2. Prepare Branch & Commit

1. Verify you are on a feature/topic branch (never directly on `main`):
   ```bash
   git status
   ```
2. Stage relevant files and create a concise, semantic commit:
   ```bash
   git add <files>
   git commit -m "<type>: <concise description>"
   ```
3. Push branch to GitHub:
   ```bash
   git push -u origin <branch-name>
   ```

## 3. Generate PR Body using `pr-template`

Format the PR body adhering strictly to this layout:

```markdown
## Summary

<diagram, diff-sketch, call tree, or component layout>

## Evidence

- **Before:** <previous behavior, failing test, or absent capability>
  **After:** <passing test output, terminal run, or UI behavior>

## Merge Danger

**Door:** <one-way or two-way>
<brief description>

**Blast Radius:** <one-word description>
<potential ramifications of merge>
```

### Guidelines for Cinder:
- **Summary**: Use ASCII diagrams, sequence trees, or component hierarchy to make the architectural change immediately graspable in one read.
- **Evidence**: Include actual terminal test output (`npm test`, `cargo test`, `cargo clippy`). If visual UI changed, describe the UI state or include screenshots.
- **Merge Danger**: Classify as two-way (safe to revert, no persistent storage migration) or one-way (schema/data migration, destructive changes). Detail blast radius (e.g. `Isolated`, `Dashboard`, `Collector`, `Packaging`).

## 4. Open the Pull Request

Use the GitHub CLI (`gh`):

```bash
gh pr create --title "<type>: <concise title>" --body-file <path-to-pr-body.md>
```

Or pass `--body "..."` directly.

## 5. Link & Verify

1. If running inside T3 Code, register the PR link with `link_pull_request`.
2. Inspect CI workflow status on GitHub:
   ```bash
   gh pr checks
   ```
3. Ensure all CI jobs (`Frontend Tests`, `Rust Checks & Tests`) complete green.
