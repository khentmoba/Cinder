//! What the user pays for an agent's usage.
//!
//! Cinder prices every token against public list prices, so the figure it
//! shows for a flat-rate plan is the *value of the work delivered*, not a
//! bill. The two only become comparable once the user says what their plan
//! costs, so this module holds what they declared and does the arithmetic.
//!
//! A plan and a budget are different things and are kept apart:
//!
//! * a **plan** is a flat rate the user pays. The work is worth more than the
//!   plan costs, and the limit on how much they can use is the provider's
//!   quota window, not a dollar amount.
//! * a **budget** is a limit on per-token spending, and it does burn down, so
//!   the spend is compared with the limit and can raise an alert.

use crate::Row;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How an agent's usage is paid for, as the user declared it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanKind {
    /// A flat-rate plan the user pays a fixed price for.
    Subscription,
    /// Billed per token against a monthly limit the user set.
    Budget,
}

/// One agent's declared plan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Plan {
    pub agent: String,
    pub kind: PlanKind,
    /// The plan price or the monthly budget, in dollars.
    pub limit: f64,
    /// A name for the plan, e.g. "Claude Pro".
    pub label: String,
}

/// What one declared plan looks like against the usage it covered.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PlanStatus {
    pub agent: String,
    pub kind: PlanKind,
    pub label: String,
    /// The plan price or the monthly budget, in dollars.
    pub limit: f64,
    /// The equivalent value of the work, or the spend for a budget.
    pub used: f64,
    /// `used` as a percentage of `limit`.
    pub percent: f64,
    /// For a plan, how many times the plan price the work was worth. For a
    /// budget this is 1 once the limit is reached.
    pub multiple: f64,
}

/// Where the declarations live.
pub fn plans_file(home: &Path) -> PathBuf {
    home.join(".cinder").join("plans.json")
}

/// Read the declarations, if there are any.
pub fn load(home: &Path) -> Vec<Plan> {
    let Some(v) = crate::read_json_file(&plans_file(home)) else {
        return Vec::new();
    };
    serde_json::from_value(v).unwrap_or_default()
}

/// Write the declarations, creating the folder if it is not there yet.
pub fn save(home: &Path, plans: &[Plan]) -> Result<(), String> {
    let path = plans_file(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // A plan with no limit says nothing, so it is not worth keeping.
    let kept: Vec<&Plan> = plans.iter().filter(|p| p.limit > 0.0).collect();
    let text = serde_json::to_string_pretty(&kept).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

/// The equivalent value of the work an agent did since the start of `month`.
///
/// Only the cost that is actually known counts: an unpriced run has no
/// equivalent value yet, and pretending otherwise would understate the
/// figure.
pub fn month_value(rows: &[Row], agent: &str, month: &str) -> f64 {
    rows.iter()
        .filter(|r| r.agent == agent && r.day.starts_with(month) && r.cost_known)
        .map(|r| r.cost)
        .sum()
}

/// The month a day belongs to, "YYYY-MM".
pub fn month_of(day: &str) -> String {
    day.chars().take(7).collect()
}

/// How each declared plan looks against the usage it covered.
pub fn statuses(rows: &[Row], plans: &[Plan], month: &str) -> Vec<PlanStatus> {
    plans
        .iter()
        .map(|p| {
            let used = month_value(rows, &p.agent, month);
            let percent = if p.limit > 0.0 { (used / p.limit * 100.0).clamp(0.0, 1000.0) } else { 0.0 };
            PlanStatus {
                agent: p.agent.clone(),
                kind: p.kind,
                label: p.label.clone(),
                limit: p.limit,
                used,
                percent,
                multiple: if p.limit > 0.0 { used / p.limit } else { 0.0 },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(kind: PlanKind, limit: f64) -> Plan {
        Plan { agent: "Claude Code".into(), kind, limit, label: "Pro".into() }
    }

    #[test]
    fn a_plan_reports_the_value_the_work_was_worth() {
        let rows = vec![
            Row { day: "2026-10-02".into(), agent: "Claude Code".into(), cost: 40.0, cost_known: true, ..Default::default() },
            Row { day: "2026-10-20".into(), agent: "Claude Code".into(), cost: 60.0, cost_known: true, ..Default::default() },
            // A different month, and an unpriced run: neither counts.
            Row { day: "2026-09-20".into(), agent: "Claude Code".into(), cost: 500.0, cost_known: true, ..Default::default() },
            Row { day: "2026-10-21".into(), agent: "Claude Code".into(), cost: 9.0, cost_known: false, ..Default::default() },
            // Another agent.
            Row { day: "2026-10-21".into(), agent: "Codex".into(), cost: 7.0, cost_known: true, ..Default::default() },
        ];
        let s = statuses(&rows, &[plan(PlanKind::Subscription, 20.0)], "2026-10");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].used, 100.0);
        assert_eq!(s[0].percent, 500.0);
        // The work was worth five times the plan price.
        assert_eq!(s[0].multiple, 5.0);
    }

    #[test]
    fn a_budget_reads_as_the_share_of_the_limit_spent() {
        let rows = vec![Row { day: "2026-10-21".into(), agent: "OpenCode".into(), cost: 25.0, cost_known: true, ..Default::default() }];
        let s = statuses(&rows, &[Plan { agent: "OpenCode".into(), kind: PlanKind::Budget, limit: 50.0, label: "monthly".into() }], "2026-10");
        assert_eq!(s[0].percent, 50.0);
        assert_eq!(s[0].multiple, 0.5);
    }

    #[test]
    fn a_declaration_without_a_limit_is_not_kept() {
        let home = std::env::temp_dir().join("cinder-plans-save");
        let _ = std::fs::remove_dir_all(&home);
        save(&home, &[plan(PlanKind::Subscription, 0.0), plan(PlanKind::Budget, 10.0)]).unwrap();
        let back = load(&home);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].limit, 10.0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_month_is_the_first_seven_characters() {
        assert_eq!(month_of("2026-10-21"), "2026-10");
    }
}
