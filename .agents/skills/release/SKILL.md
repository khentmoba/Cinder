---
name: release
description: Release a new version of Cinder. Handles version bumps, test verification, building artifacts, creating Git tags, and publishing GitHub Releases for native in-app updates. Use when preparing or publishing a Cinder release.
---

# Cinder Release Runbook

Release Cinder desktop application and trigger the native in-app update pipeline.

## 1. Version Bump & Sync

Ensure working tree is clean. Cinder's version lives in three files that must stay in sync:

1. `package.json` -> `"version": "x.y.z"`
2. `src-tauri/Cargo.toml` -> `version = "x.y.z"`
3. `src-tauri/tauri.conf.json` -> `"version": "x.y.z"`

```bash
# Example: Bumping to 0.2.0
# Update all three files to the new semantic version
```

## 2. Verification Gate

Run all unit tests, linters, and integration checks before building release artifacts:

```bash
# Frontend tests (dashboard & updater logic)
npm test

# Rust tests (parsers, watchers, updater, pricing)
cargo test --manifest-path src-tauri/Cargo.toml

# Clippy linter (must pass with zero warnings)
cargo clippy --manifest-path src-tauri/Cargo.toml --tests -- -D warnings
```

Do not proceed if any check fails.

## 3. Local Release Build (Optional Validation)

To build and test the Windows NSIS installer locally:

```bash
npm run build
# The compiled installer is output to:
# src-tauri/target/release/bundle/nsis/Cinder_<version>_x64-setup.exe

# To test installing locally:
npm run install-local
```

## 4. Git Commit, Tag & Push

Once tests pass and versions are synced:

```bash
git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json src-tauri/Cargo.lock
git commit -m "Release vX.Y.Z"
git push origin main

# Create and push the release tag
git tag vX.Y.Z
git push origin vX.Y.Z
```

## 5. Automated CI Release & Verification

Pushing the `v*` tag triggers the GitHub Actions workflow (`.github/workflows/release.yml`), which:
1. Checks out the tagged commit.
2. Runs the test suite (`npm test`, `cargo test`).
3. Compiles the Windows NSIS installer (`npm run build`).
4. Publishes a GitHub Release with `Cinder_<version>_x64-setup.exe` attached.

### Manual CLI Release Fallback

If GitHub Actions is unavailable or a manual upload is preferred:

```bash
gh release create vX.Y.Z src-tauri/target/release/bundle/nsis/*.exe \
  --title "Cinder vX.Y.Z" \
  --generate-notes
```

## 6. Post-Release Verification

1. Check `gh release view vX.Y.Z` or open `https://github.com/khentmoba/Cinder/releases`.
2. Confirm `Cinder_<version>_x64-setup.exe` is attached to the release.
3. Open an existing Cinder install:
   - Confirm the update badge appears (`Update vX.Y.Z`).
   - Open update modal and verify release notes and version diff.
   - Click **Update Now** to verify in-app streaming download and silent relaunch.
