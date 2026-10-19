// Cinder update system — checks GitHub releases and updates the app natively.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tauri::{AppHandle, Emitter};

pub const DEFAULT_REPO: &str = "khentmoba/Cinder";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_name: String,
    pub release_notes: String,
    pub published_at: String,
    pub asset_name: Option<String>,
    pub asset_size: Option<u64>,
    pub download_url: Option<String>,
    pub release_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
    pub percent: f64,
    pub finished: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub published_at: Option<String>,
    pub html_url: String,
    pub draft: Option<bool>,
    #[allow(dead_code)]
    pub prerelease: Option<bool>,
    #[serde(default)]
    pub assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

/// Normalizes version string by stripping 'v' or 'V' prefixes and whitespace.
pub fn clean_version_str(ver: &str) -> &str {
    ver.trim().trim_start_matches(['v', 'V'])
}

/// Compares two version strings (semver compliant, with fallback for loose numbers).
pub fn is_newer_version(latest: &str, current: &str) -> bool {
    let clean_lat = clean_version_str(latest);
    let clean_cur = clean_version_str(current);

    if let (Ok(lat_ver), Ok(cur_ver)) = (
        semver::Version::parse(clean_lat),
        semver::Version::parse(clean_cur),
    ) {
        return lat_ver > cur_ver;
    }

    // Fallback: compare dot-separated numbers e.g. "0.2" or "1.0.0.1"
    let parse_parts = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|part| {
                part.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .collect()
    };
    let lat_parts = parse_parts(clean_lat);
    let cur_parts = parse_parts(clean_cur);
    lat_parts > cur_parts
}

/// Finds the best installer or executable asset for the platform.
pub fn find_best_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    // 1. Look for installer setup exe (e.g. Cinder_0.2.0_x64-setup.exe)
    if let Some(asset) = assets.iter().find(|a| {
        let lower = a.name.to_lowercase();
        lower.ends_with(".exe") && (lower.contains("setup") || lower.contains("installer"))
    }) {
        return Some(asset);
    }
    // 2. Look for any .exe executable
    if let Some(asset) = assets.iter().find(|a| a.name.to_lowercase().ends_with(".exe")) {
        return Some(asset);
    }
    // 3. Fallback to any asset
    assets.first()
}

/// Evaluates a list of releases against the current version.
pub fn evaluate_releases(releases: Vec<GithubRelease>, current_ver: &str, repo: &str) -> UpdateInfo {
    let latest_rel = releases.into_iter().find(|r| !r.draft.unwrap_or(false));

    match latest_rel {
        Some(rel) => {
            let latest_version = clean_version_str(&rel.tag_name).to_string();
            let newer = is_newer_version(&rel.tag_name, current_ver);
            let best_asset = find_best_asset(&rel.assets);

            UpdateInfo {
                available: newer && best_asset.is_some(),
                current_version: current_ver.to_string(),
                latest_version,
                release_name: rel.name.unwrap_or_else(|| rel.tag_name.clone()),
                release_notes: rel.body.unwrap_or_default(),
                published_at: rel.published_at.unwrap_or_default(),
                asset_name: best_asset.map(|a| a.name.clone()),
                asset_size: best_asset.map(|a| a.size),
                download_url: best_asset.map(|a| a.browser_download_url.clone()),
                release_url: rel.html_url,
            }
        }
        None => UpdateInfo {
            available: false,
            current_version: current_ver.to_string(),
            latest_version: current_ver.to_string(),
            release_name: "No releases found".to_string(),
            release_notes: String::new(),
            published_at: String::new(),
            asset_name: None,
            asset_size: None,
            download_url: None,
            release_url: format!("https://github.com/{}", repo),
        },
    }
}

/// Fetches releases from GitHub and checks for updates.
pub async fn check_for_updates_impl(current_ver: &str, repo: &str) -> Result<UpdateInfo, String> {
    let client = reqwest::Client::builder()
        .user_agent("Cinder-Desktop-App")
        .build()
        .map_err(|e| e.to_string())?;

    let url = format!("https://api.github.com/repos/{}/releases", repo);
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| format!("Failed to reach GitHub releases: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned status {}", resp.status()));
    }

    let releases: Vec<GithubRelease> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse GitHub releases response: {}", e))?;

    Ok(evaluate_releases(releases, current_ver, repo))
}

/// Downloads the update file with progress and triggers installation.
pub async fn download_and_install_impl(
    app: AppHandle,
    download_url: String,
    asset_name: Option<String>,
) -> Result<(), String> {
    use std::io::Write;

    let client = reqwest::Client::builder()
        .user_agent("Cinder-Desktop-App")
        .build()
        .map_err(|e| e.to_string())?;

    let mut res = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {}", e))?;

    if !res.status().is_success() {
        return Err(format!("Download failed with HTTP status {}", res.status()));
    }

    let total = res.content_length().unwrap_or(0);
    let temp_dir = std::env::temp_dir();
    let filename = asset_name.unwrap_or_else(|| "cinder-update-setup.exe".to_string());
    let dest_path = temp_dir.join(format!("cinder-update-{}", filename));

    let mut file = fs::File::create(&dest_path)
        .map_err(|e| format!("Failed to create temp installer file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = res.chunk().await.map_err(|e| format!("Error downloading chunk: {}", e))? {
        file.write_all(&chunk)
            .map_err(|e| format!("Error writing update chunk: {}", e))?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed() >= std::time::Duration::from_millis(100) || (total > 0 && downloaded >= total) {
            let percent = if total > 0 {
                (downloaded as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let _ = app.emit(
                "update-download-progress",
                DownloadProgress {
                    downloaded,
                    total,
                    percent,
                    finished: false,
                },
            );
            last_emit = std::time::Instant::now();
        }
    }

    file.flush()
        .map_err(|e| format!("Failed to flush installer file: {}", e))?;
    drop(file);

    let _ = app.emit(
        "update-download-progress",
        DownloadProgress {
            downloaded,
            total: downloaded,
            percent: 100.0,
            finished: true,
        },
    );

    // Give UI time to render 100% completion before launching installer
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    install_and_restart(&dest_path, &app)
}

/// Generates the Windows cmd script used to apply an update and relaunch Cinder.
pub fn build_update_script(installer_path: &Path, target_exe: &Path, is_setup: bool) -> String {
    if is_setup && installer_path.to_string_lossy().to_lowercase().contains("setup") {
        // NSIS setup installer: run with /S (silent) and /UPDATE (in-place update mode)
        format!(
            "@echo off\r\n\
             ping 127.0.0.1 -n 3 >nul\r\n\
             taskkill /F /IM cinder.exe >nul 2>&1\r\n\
             start /wait \"\" \"{}\" /S /UPDATE\r\n\
             start \"\" \"{}\"\r\n\
             del \"{}\" >nul 2>&1\r\n\
             (goto) 2>nul & del \"%~f0\"\r\n",
            installer_path.display(),
            target_exe.display(),
            installer_path.display(),
        )
    } else {
        // Direct executable replacement: replace target_exe and relaunch
        format!(
            "@echo off\r\n\
             ping 127.0.0.1 -n 3 >nul\r\n\
             taskkill /F /IM cinder.exe >nul 2>&1\r\n\
             copy /y \"{}\" \"{}\" >nul 2>&1\r\n\
             start \"\" \"{}\"\r\n\
             del \"{}\" >nul 2>&1\r\n\
             (goto) 2>nul & del \"%~f0\"\r\n",
            installer_path.display(),
            target_exe.display(),
            target_exe.display(),
            installer_path.display(),
        )
    }
}

#[cfg(windows)]
pub fn install_and_restart(installer_path: &Path, app: &AppHandle) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let installed_exe = dirs::data_local_dir().map(|p| p.join("Cinder").join("cinder.exe"));
    let target_exe = installed_exe
        .as_ref()
        .filter(|p| p.exists())
        .cloned()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| installed_exe.unwrap_or_else(|| std::path::PathBuf::from("cinder.exe")));

    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("cinder-update-apply.cmd");

    let is_setup = installer_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase().contains("setup") || s.to_lowercase().ends_with(".exe"))
        .unwrap_or(true);

    let script_content = build_update_script(installer_path, &target_exe, is_setup);

    fs::write(&script_path, script_content)
        .map_err(|e| format!("Failed to write update script: {}", e))?;

    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;

    std::process::Command::new("cmd.exe")
        .args(["/c", script_path.to_str().unwrap_or_default()])
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("Failed to launch updater process: {}", e))?;

    app.exit(0);
    Ok(())
}

#[cfg(not(windows))]
pub fn install_and_restart(installer_path: &Path, app: &AppHandle) -> Result<(), String> {
    let _ = opener_or_launch(installer_path);
    app.exit(0);
    Ok(())
}

#[cfg(not(windows))]
fn opener_or_launch(path: &Path) -> std::io::Result<std::process::Child> {
    std::process::Command::new("open").arg(path).spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparisons() {
        assert!(is_newer_version("0.2.0", "0.1.0"));
        assert!(is_newer_version("v0.2.0", "0.1.0"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(is_newer_version("v0.1.1", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("v0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
        assert!(!is_newer_version("v0.0.9", "0.1.0"));
    }

    #[test]
    fn test_find_best_asset() {
        let assets = vec![
            GithubAsset {
                name: "source.zip".to_string(),
                size: 1000,
                browser_download_url: "https://example.com/source.zip".to_string(),
            },
            GithubAsset {
                name: "Cinder_0.2.0_x64-setup.exe".to_string(),
                size: 25000000,
                browser_download_url: "https://example.com/setup.exe".to_string(),
            },
            GithubAsset {
                name: "cinder.exe".to_string(),
                size: 15000000,
                browser_download_url: "https://example.com/cinder.exe".to_string(),
            },
        ];

        let best = find_best_asset(&assets).expect("should find asset");
        assert_eq!(best.name, "Cinder_0.2.0_x64-setup.exe");
    }

    #[test]
    fn test_evaluate_releases() {
        let releases = vec![
            GithubRelease {
                tag_name: "v0.2.0".to_string(),
                name: Some("Cinder 0.2.0".to_string()),
                body: Some("Fixed bugs and added features".to_string()),
                published_at: Some("2026-10-19T12:00:00Z".to_string()),
                html_url: "https://github.com/khentmoba/Cinder/releases/tag/v0.2.0".to_string(),
                draft: Some(false),
                prerelease: Some(false),
                assets: vec![GithubAsset {
                    name: "Cinder_0.2.0_x64-setup.exe".to_string(),
                    size: 25000000,
                    browser_download_url: "https://github.com/khentmoba/Cinder/releases/download/v0.2.0/Cinder_0.2.0_x64-setup.exe".to_string(),
                }],
            },
        ];

        let info = evaluate_releases(releases, "0.1.0", "khentmoba/Cinder");
        assert!(info.available);
        assert_eq!(info.latest_version, "0.2.0");
        assert_eq!(info.current_version, "0.1.0");
        assert_eq!(info.asset_name.as_deref(), Some("Cinder_0.2.0_x64-setup.exe"));
    }

    #[test]
    fn test_build_update_script_runs_setup_with_update_flag() {
        let installer = Path::new("C:\\temp\\Cinder_0.2.0_x64-setup.exe");
        let target = Path::new("C:\\Users\\Admin\\AppData\\Local\\Cinder\\cinder.exe");
        let script = build_update_script(installer, target, true);
        assert!(script.contains("/S /UPDATE"));
        assert!(script.contains("cinder.exe"));
    }

    #[test]
    fn test_evaluate_releases_when_current_is_latest() {
        let releases = vec![
            GithubRelease {
                tag_name: "v0.1.0".to_string(),
                name: Some("Cinder 0.1.0".to_string()),
                body: Some("Initial release".to_string()),
                published_at: Some("2026-10-19T12:00:00Z".to_string()),
                html_url: "https://github.com/khentmoba/Cinder/releases/tag/v0.1.0".to_string(),
                draft: Some(false),
                prerelease: Some(false),
                assets: vec![],
            },
        ];

        let info = evaluate_releases(releases, "0.1.0", "khentmoba/Cinder");
        assert!(!info.available);
        assert_eq!(info.latest_version, "0.1.0");
    }

    #[test]
    fn test_evaluate_empty_releases() {
        let info = evaluate_releases(vec![], "0.1.0", "khentmoba/Cinder");
        assert!(!info.available);
        assert_eq!(info.latest_version, "0.1.0");
    }
}
