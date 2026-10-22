//! Alerts: the things worth interrupting the user about.
//!
//! Two things can run out: a provider's quota window, and a budget the user
//! set. Both are evaluated here, from the same numbers the dashboard shows,
//! so the notification and the panel never disagree.
//!
//! A window is projected forward from how fast it is being consumed: a window
//! that is 40% used a tenth of the way through will not last. The projection
//! needs the window to have been under way for a little while, because a
//! window that has just opened says nothing about how it will end.

use crate::budget::{PlanKind, PlanStatus};
use crate::quota::LiveQuota;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::LazyLock;

/// How serious an alert is.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warn,
    Critical,
}

/// One thing worth telling the user about.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Alert {
    /// A stable id, so the same alert is not announced twice.
    pub id: String,
    pub agent: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
}

/// How far through its window a quota window is, as a fraction.
///
/// A window with no known length or no known reset time cannot be projected,
/// and reports zero.
pub fn elapsed_fraction(window_minutes: i64, resets_at: i64, now: i64) -> f64 {
    let span = window_minutes * 60;
    if span <= 0 || resets_at <= 0 {
        return 0.0;
    }
    let left = resets_at - now;
    (1.0 - (left as f64 / span as f64)).clamp(0.0, 1.0)
}

/// The alert a live quota window raises, if any.
pub fn quota_alert(window: &LiveQuota, now: i64) -> Option<Alert> {
    let pct = window.used_percent;
    let id = format!("quota:{}:{}", window.agent, window.window);
    let what = format!("{} {} window", window.agent, window.window);

    // A window that is nearly spent is worth saying so whatever the pace.
    if pct >= 90.0 {
        return Some(Alert {
            id,
            agent: window.agent.clone(),
            severity: Severity::Critical,
            title: format!("{what} is {:.0}% used", pct),
            detail: window.detail.clone(),
        });
    }

    let elapsed = elapsed_fraction(window.window_minutes, window.resets_at, now);
    // Too early to project: a window that has barely opened says nothing
    // about how it will end, and an even pace in the first minutes would
    // look like a warning on every window at once.
    if elapsed < 0.15 {
        return None;
    }
    let projected = pct / elapsed;
    // Half a percentage point of slack: a window used at exactly the even pace
    // projects to a hundred, and that is "on pace to use the whole window",
    // not "will run out before it resets".
    if projected > 100.5 {
        Some(Alert {
            id,
            agent: window.agent.clone(),
            severity: Severity::Critical,
            title: format!("{what} will run out before it resets"),
            detail: format!("{:.0}% used this early — on pace for {:.0}%", pct, projected),
        })
    } else if projected >= 80.0 {
        Some(Alert {
            id,
            agent: window.agent.clone(),
            severity: Severity::Warn,
            title: format!("{what} is on pace to use the whole window"),
            detail: format!("{:.0}% used, on pace for {:.0}%", pct, projected),
        })
    } else {
        None
    }
}

/// The alert a budget raises, if any.
///
/// A plan is not a budget: its price says what the user pays, not how much
/// they may use, so it never raises an alert. The provider's quota window is
/// the limit for a plan.
pub fn budget_alert(status: &PlanStatus) -> Option<Alert> {
    if status.kind != PlanKind::Budget || status.limit <= 0.0 {
        return None;
    }
    let id = format!("budget:{}", status.agent);
    let detail = format!("{} of {}", money(status.used), money(status.limit));
    if status.percent >= 100.0 {
        Some(Alert {
            id,
            agent: status.agent.clone(),
            severity: Severity::Critical,
            title: format!("{} is over its budget", status.agent),
            detail,
        })
    } else if status.percent >= 80.0 {
        Some(Alert {
            id,
            agent: status.agent.clone(),
            severity: Severity::Warn,
            title: format!("{} is at {:.0}% of its budget", status.agent, status.percent),
            detail,
        })
    } else {
        None
    }
}

/// A dollar amount, with enough decimals to say something about a small one.
fn money(v: f64) -> String {
    if v > 0.0 && v < 0.01 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

/// Every alert that the current numbers justify.
pub fn evaluate(windows: &[LiveQuota], budgets: &[PlanStatus], now: i64) -> Vec<Alert> {
    let mut out: Vec<Alert> = Vec::new();
    for w in windows {
        if let Some(a) = quota_alert(w, now) {
            out.push(a);
        }
    }
    for b in budgets {
        if let Some(a) = budget_alert(b) {
            out.push(a);
        }
    }
    // The worst first, so the header badge can show the first one.
    out.sort_by_key(|a| std::cmp::Reverse(severity_rank(a.severity)));
    out
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Warn => 1,
        Severity::Critical => 2,
    }
}

/// The alerts that were already announced, so the same one is not repeated.
static ANNOUNCED: LazyLock<std::sync::Mutex<HashSet<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// The alerts that have not been announced yet.
///
/// An alert that clears is forgotten, so it can fire again the next time it
/// comes back.
pub fn take_new(alerts: &[Alert]) -> Vec<Alert> {
    let mut seen = ANNOUNCED.lock().unwrap_or_else(|e| e.into_inner());
    let current: HashSet<String> = alerts.iter().map(|a| a.id.clone()).collect();
    seen.retain(|id| current.contains(id));
    let mut fresh = Vec::new();
    for a in alerts {
        if !seen.contains(&a.id) {
            seen.insert(a.id.clone());
            fresh.push(a.clone());
        }
    }
    fresh
}

/// Forget every announced alert, so the next round announces everything again.
pub fn reset_announcements() {
    ANNOUNCED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(percent: f64, minutes: i64, resets_at: i64) -> LiveQuota {
        LiveQuota {
            agent: "Claude Code".into(),
            window: "5h".into(),
            used_percent: percent,
            window_minutes: minutes,
            resets_at,
            plan: "pro".into(),
            detail: "5-hour session window".into(),
            used_amount: 0.0,
            limit_amount: 0.0,
            balance: 0.0,
        }
    }

    #[test]
    fn a_window_used_faster_than_it_fills_raises_an_alert() {
        let now = 1_000_000;
        let span = 300 * 60;
        // 40% used with a fifth of the window gone: on pace for 200%.
        let a = quota_alert(&window(40.0, 300, now + span * 4 / 5), now).unwrap();
        assert_eq!(a.severity, Severity::Critical);
        assert!(a.title.contains("run out"));
    }

    #[test]
    fn a_window_used_at_an_even_pace_only_warns() {
        let now = 1_000_000;
        let span = 300 * 60;
        // 20% used with a fifth of the window gone: exactly on pace, so it
        // will use the whole window but not run out before the reset.
        let a = quota_alert(&window(20.0, 300, now + span * 4 / 5), now).unwrap();
        assert_eq!(a.severity, Severity::Warn);
        assert!(a.title.contains("whole window"));
    }

    #[test]
    fn a_window_used_well_within_the_pace_says_nothing() {
        let now = 1_000_000;
        let span = 300 * 60;
        // 10% used with 40% of the window gone: on pace for 25%.
        assert!(quota_alert(&window(10.0, 300, now + span * 3 / 5), now).is_none());
    }

    #[test]
    fn a_window_that_just_opened_says_nothing() {
        let now = 1_000_000;
        // 2% used two minutes into a five-hour window: too early to project.
        let span = 300 * 60;
        assert!(quota_alert(&window(2.0, 300, now + span - 120), now).is_none());
    }

    #[test]
    fn a_window_that_is_nearly_spent_raises_an_alert() {
        let now = 1_000_000;
        let a = quota_alert(&window(95.0, 300, now + 300), now).unwrap();
        assert_eq!(a.severity, Severity::Critical);
        assert!(a.title.contains("95%"));
    }

    #[test]
    fn a_plan_never_raises_a_budget_alert() {
        let status = PlanStatus {
            agent: "Claude Code".into(),
            kind: PlanKind::Subscription,
            label: "Pro".into(),
            limit: 20.0,
            used: 500.0,
            percent: 2500.0,
            multiple: 25.0,
        };
        assert!(budget_alert(&status).is_none());
    }

    #[test]
    fn a_budget_raises_an_alert_when_it_is_nearly_spent() {
        let status = PlanStatus {
            agent: "OpenCode".into(),
            kind: PlanKind::Budget,
            label: "monthly".into(),
            limit: 50.0,
            used: 42.0,
            percent: 84.0,
            multiple: 0.84,
        };
        let a = budget_alert(&status).unwrap();
        assert_eq!(a.severity, Severity::Warn);
        assert!(a.detail.contains("42.00"));
        assert!(a.detail.contains("50.00"));
    }

    #[test]
    fn a_small_amount_still_says_something() {
        let status = PlanStatus {
            agent: "OpenCode".into(),
            kind: PlanKind::Budget,
            label: "monthly".into(),
            limit: 0.001,
            used: 0.00225,
            percent: 225.0,
            multiple: 2.25,
        };
        let a = budget_alert(&status).unwrap();
        // "$0.00 of $0.00" would say nothing.
        assert!(!a.detail.contains("$0.00 of"), "detail {}", a.detail);
        assert!(a.detail.contains("of $0.0010"), "detail {}", a.detail);
    }

    #[test]
    fn an_alert_is_announced_once_and_again_after_it_clears() {
        reset_announcements();
        let a = Alert {
            id: "quota:Claude Code:5h".into(),
            agent: "Claude Code".into(),
            severity: Severity::Warn,
            title: "t".into(),
            detail: "d".into(),
        };
        assert_eq!(take_new(std::slice::from_ref(&a)).len(), 1);
        // The same alert is not announced a second time.
        assert!(take_new(std::slice::from_ref(&a)).is_empty());
        // Once it clears, it can fire again.
        assert!(take_new(&[]).is_empty());
        assert_eq!(take_new(std::slice::from_ref(&a)).len(), 1);
        reset_announcements();
    }
}
