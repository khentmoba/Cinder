//! The system tray icon and the popover it opens.
//!
//! The tray is the cheapest place to see what the subscriptions are doing.
//! The tooltip carries the tightest live window, the number next to the icon
//! says how much of it is gone, and a click opens a small window next to the
//! icon that shows every provider, what is left of each window, when it
//! resets, and what today cost.
//!
//! The popover is a plain window that is shown, placed and hidden from here,
//! so the JavaScript in it only ever reads numbers and asks for an action.

use crate::alerts::Alert;
use crate::budget::PlanStatus;
use crate::quota::{self, LiveQuota, QuotaReport};
use serde::Serialize;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WindowEvent};

/// The label of the popover window.
pub const POPOVER: &str = "tray";
/// The id of the tray icon.
pub const TRAY_ID: &str = "cinder-tray";
/// The gap between the popover and the edge of the screen.
const MARGIN: i32 = 12;

/// How long after a tray click a blur is ignored.
///
/// Clicking the tray icon takes the focus away from the popover, which would
/// otherwise close it before the click could be acted on.
const BLUR_GRACE_MS: i64 = 400;

// ------------------------------------------------------------------- the data

/// One provider, as the popover shows it.
#[derive(Serialize, Clone, Debug, Default)]
pub struct TrayCard {
    pub agent: String,
    /// Which account this is, when a machine holds more than one.
    pub account: String,
    /// The plan the account is on, e.g. "max", "pro", "plus".
    pub plan: String,
    /// The windows this provider reported.
    pub windows: Vec<LiveQuota>,
    /// Why the provider could not be read.
    pub error: String,
    pub error_kind: Option<quota::ErrorKind>,
    /// True when the windows are the last known good ones.
    pub stale: bool,
    /// Unix seconds of the last successful read.
    pub updated_at: i64,
}

/// What the popover shows.
#[derive(Serialize, Clone, Debug, Default)]
pub struct TraySummary {
    /// One card per provider that is signed in on this machine.
    pub cards: Vec<TrayCard>,
    /// The things worth interrupting the user about.
    pub alerts: Vec<Alert>,
    /// The plans the user declared.
    pub plans: Vec<PlanStatus>,
    /// The equivalent value of today's work, in dollars.
    pub today_cost: f64,
    /// True when every dollar of today's work could be priced.
    pub today_cost_known: bool,
    /// True when some of today's work could not be priced.
    pub today_cost_partial: bool,
    pub today_tokens: u64,
    pub today_requests: u64,
    /// When the live quotas were last read (ISO-8601, UTC).
    pub checked_at: String,
    /// True when the numbers came off the disk rather than off the network.
    pub restored: bool,
    /// When the local logs were last scanned (ISO-8601, UTC).
    pub scanned_at: String,
}

/// What the last scan found, kept so the popover can answer without one.
#[derive(Serialize, Clone, Debug, Default)]
pub struct ScanSummary {
    pub today_cost: f64,
    pub today_cost_known: bool,
    pub today_cost_partial: bool,
    pub today_tokens: u64,
    pub today_requests: u64,
    pub alerts: Vec<Alert>,
    pub plans: Vec<PlanStatus>,
    pub scanned_at: String,
}

static SCAN: LazyLock<Mutex<ScanSummary>> = LazyLock::new(|| Mutex::new(ScanSummary::default()));

/// Remember what the last scan found.
pub fn remember(summary: ScanSummary) {
    let mut guard = SCAN.lock().unwrap_or_else(|e| e.into_inner());
    *guard = summary;
}

/// The cards for the providers that are signed in on this machine.
///
/// A provider that is not signed in gets no card: an empty card is noise, and
/// the point of the popover is to show what is actually running.
fn cards(reports: &[QuotaReport]) -> Vec<TrayCard> {
    reports
        .iter()
        .filter(|r| !r.skipped)
        .map(|r| TrayCard {
            agent: r.agent.clone(),
            account: r.account.clone(),
            plan: r.plan.clone(),
            windows: r.windows.clone(),
            error: r.error.clone(),
            error_kind: r.error_kind,
            stale: r.stale,
            updated_at: r.updated_at,
        })
        .collect()
}

/// The whole summary: the live quota on top of what the last scan found.
pub fn summary() -> TraySummary {
    let (reports, checked_at, _, restored) = quota::snapshot();
    let scan = SCAN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    TraySummary {
        cards: cards(&reports),
        alerts: scan.alerts,
        plans: scan.plans,
        today_cost: scan.today_cost,
        today_cost_known: scan.today_cost_known,
        today_cost_partial: scan.today_cost_partial,
        today_tokens: scan.today_tokens,
        today_requests: scan.today_requests,
        checked_at,
        restored,
        scanned_at: scan.scanned_at,
    }
}

// ----------------------------------------------------------------- the window

/// When the tray icon was last clicked, in unix milliseconds.
static LAST_CLICK: AtomicI64 = AtomicI64::new(0);

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Show the dashboard.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Hide the popover, if it is open.
pub fn hide_popover(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER) {
        let _ = window.hide();
    }
}

/// Whether the popover is open right now.
pub fn popover_visible(app: &AppHandle) -> bool {
    app.get_webview_window(POPOVER).and_then(|w| w.is_visible().ok()).unwrap_or(false)
}

/// Where the tray icon is, in physical pixels, when the platform can say.
///
/// The rectangle comes back as a logical or a physical one, depending on the
/// platform, so both are read the same way here.
fn icon_rect(tray: Option<&TrayIcon>) -> Option<(i32, i32, i32, i32)> {
    let rect = tray?.rect().ok().flatten()?;
    let (x, y) = match rect.position {
        tauri::Position::Physical(p) => (p.x, p.y),
        tauri::Position::Logical(p) => (p.x as i32, p.y as i32),
    };
    let (w, h) = match rect.size {
        tauri::Size::Physical(s) => (s.width as i32, s.height as i32),
        tauri::Size::Logical(s) => (s.width as i32, s.height as i32),
    };
    Some((x, y, w, h))
}

/// Put the popover next to the tray icon, inside the screen.
///
/// The icon's own rectangle is used when the platform can tell us where it
/// is; otherwise the popover hangs from the bottom-right of the work area,
/// which is where the notification area lives on Windows.
fn place(app: &AppHandle, tray: Option<&TrayIcon>) {
    let Some(window) = app.get_webview_window(POPOVER) else { return };
    let Ok(size) = window.outer_size() else { return };
    let (pw, ph) = (size.width as i32, size.height as i32);

    let icon = icon_rect(tray);

    // The monitor the icon sits on, so the popover lands on the same screen.
    let monitor = icon
        .and_then(|(x, y, _, _)| window.monitor_from_point(x as f64, y as f64).ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten());

    let Some(monitor) = monitor else {
        let _ = window.set_position(PhysicalPosition::new(MARGIN, MARGIN));
        return;
    };
    let area = monitor.work_area();
    let (ax, ay) = (area.position.x, area.position.y);
    let (aw, ah) = (area.size.width as i32, area.size.height as i32);

    let max_x = (ax + aw - pw).max(ax);
    let x = match icon {
        // The right edge of the popover lines up with the icon.
        Some((ix, _, iw, _)) => (ix + iw - pw).clamp(ax, max_x),
        None => (max_x - MARGIN).max(ax),
    };
    // The popover sits on the bottom edge of the work area, which is just
    // above the taskbar.
    let y = ay + ah - ph;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Open the popover if it is closed, close it if it is open.
pub fn toggle(app: &AppHandle, tray: Option<&TrayIcon>) {
    let visible = popover_visible(app);
    debug(&format!("toggle, visible={visible}"));
    if visible {
        hide_popover(app);
        return;
    }
    show(app, tray);
}

/// Show the popover, wherever it belongs.
fn show(app: &AppHandle, tray: Option<&TrayIcon>) {
    debug("showing the popover");
    if let Some(window) = app.get_webview_window(POPOVER) {
        place(app, tray);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Print something when the app runs with CINDER_DEBUG set.
fn debug(message: &str) {
    if std::env::var("CINDER_DEBUG").is_ok() {
        eprintln!("{message}");
    }
}

// -------------------------------------------------------------- the tray icon

/// The tray tooltip: the tightest window first, then the rest.
pub fn tooltip(reports: &[QuotaReport]) -> String {
    let mut windows: Vec<&LiveQuota> = reports
        .iter()
        .filter(|r| !r.skipped)
        .flat_map(|r| r.windows.iter())
        .collect();
    if windows.is_empty() {
        return "Cinder — no live quota available".into();
    }
    windows.sort_by(|a, b| b.used_percent.partial_cmp(&a.used_percent).unwrap_or(std::cmp::Ordering::Equal));
    let mut parts = vec![format!("Cinder — {} windows", windows.len())];
    for w in windows.iter().take(3) {
        parts.push(format!("{} {} {:.0}%", w.agent, w.window, w.used_percent));
    }
    parts.join(" | ")
}

/// The number shown next to the icon: how much of the tightest window is gone.
pub fn title(reports: &[QuotaReport]) -> String {
    let worst = reports
        .iter()
        .filter(|r| !r.skipped)
        .flat_map(|r| r.windows.iter())
        .map(|w| w.used_percent)
        .fold(None::<f64>, |acc, p| Some(acc.map_or(p, |a| a.max(p))));
    match worst {
        Some(p) => format!("{:.0}%", p),
        None => String::new(),
    }
}

/// The tray icon, kept so its tooltip can be refreshed.
static TRAY: LazyLock<Mutex<Option<TrayIcon>>> = LazyLock::new(|| Mutex::new(None));

/// Refresh the tray tooltip and the number next to the icon.
pub fn refresh() {
    let (reports, _, _, _) = quota::snapshot();
    let tip = tooltip(&reports);
    let text = title(&reports);
    let mut guard = TRAY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tray) = guard.as_mut() {
        let _ = tray.set_tooltip(Some(tip));
        let _ = tray.set_title(if text.is_empty() { None } else { Some(text) });
    }
}

/// Build the tray icon and its menu.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open Cinder", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Rescan now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Cinder", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &refresh, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tooltip(&[]))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "refresh" => rescan(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // A left click opens the popover; the menu is for the right
            // button, so it stays out of the way.
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                LAST_CLICK.store(now_millis(), Ordering::Relaxed);
                toggle(tray.app_handle(), Some(tray));
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    if std::env::var("CINDER_DEBUG").is_ok() {
        if let Ok(rect) = tray.rect() {
            eprintln!("tray icon at {rect:?}");
        } else {
            eprintln!("the tray icon did not say where it is");
        }
    }
    let mut guard = TRAY.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(tray);
    Ok(())
}

/// Open the popover straight away, for looking at it without a mouse.
///
/// `CINDER_TRAY_POPOVER=1 cinder` does this, which is how the popover is
/// checked on a machine where nobody is going to click the tray icon.
pub fn open_on_start(app: &AppHandle) {
    if std::env::var("CINDER_TRAY_POPOVER").is_err() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // Give the tray icon a moment to appear, so the popover is placed
        // next to it rather than in the corner.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        show(&app, None);
    });
}

/// Run a scan off the UI thread and push the result into the open window.
pub fn rescan(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let stats = crate::scan(&|_| {});
        let _ = app.emit("stats-updated", &stats);
    });
}

/// The popover closes when it loses the focus, the way a menu does.
///
/// The click that opened it takes the focus away for a moment, so a blur
/// right after a tray click is ignored.
pub fn watch_popover(window: &tauri::WebviewWindow) {
    let handle = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Focused(false) => {
            let last = LAST_CLICK.load(Ordering::Relaxed);
            if now_millis() - last < BLUR_GRACE_MS {
                return;
            }
            debug("the popover lost the focus, so it closes");
            let _ = handle.hide();
        }
        WindowEvent::CloseRequested { api, .. } => {
            // Closing the popover means hiding it: the app keeps running in
            // the tray.
            api.prevent_close();
            let _ = handle.hide();
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_signed_in_provider_gets_a_card() {
        quota::publish(vec![
            QuotaReport::ok("Claude Code", vec![LiveQuota { agent: "Claude Code".into(), window: "5h".into(), used_percent: 29.0, ..Default::default() }]),
            QuotaReport::ok("Codex", vec![LiveQuota { agent: "Codex".into(), window: "5h".into(), used_percent: 48.0, ..Default::default() }]),
            QuotaReport::absent("OpenCode"),
        ]);
        let summary = summary();
        let agents: Vec<&str> = summary.cards.iter().map(|c| c.agent.as_str()).collect();
        assert_eq!(agents, vec!["Claude Code", "Codex"]);
        // A provider that is not signed in gets no card.
        assert!(!agents.contains(&"OpenCode"));
    }

    #[test]
    fn the_title_says_how_much_of_the_tightest_window_is_gone() {
        let reports = vec![
            QuotaReport::ok("Claude Code", vec![LiveQuota { used_percent: 29.0, ..Default::default() }]),
            QuotaReport::ok("Codex", vec![LiveQuota { used_percent: 86.0, ..Default::default() }]),
        ];
        assert_eq!(title(&reports), "86%");
        assert_eq!(title(&[]), "");
    }
}
