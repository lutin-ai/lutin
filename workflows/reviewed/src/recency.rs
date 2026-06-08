//! Recency sampling for the reviewers. Per-session, per-`(principle,
//! tool)` pass streaks: once a check passes its `recency_points`
//! threshold it becomes "trusted" and is re-run only every
//! `RECHECK_INTERVAL`-th opportunity; any block resets it to every-call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::types::Principle;

const FILENAME: &str = "recency.json";
const RECHECK_INTERVAL: u32 = 5;

#[derive(Debug, Default, Clone)]
struct Entry {
    pass_streak: u32,
    checks_since_run: u32,
}

#[derive(Debug, Default)]
pub struct RecencyState {
    entries: HashMap<(String, String), Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Check,
    Skip { streak: u32 },
}

#[derive(Serialize, Deserialize)]
struct Record {
    principle: String,
    tool: String,
    pass_streak: u32,
    checks_since_run: u32,
}

impl RecencyState {
    fn entry(&mut self, principle: &str, tool: &str) -> &mut Entry {
        self.entries
            .entry((principle.to_string(), tool.to_string()))
            .or_default()
    }

    pub fn decide(&mut self, principle: &Principle, tool: &str) -> Decision {
        let points = principle.recency_points;
        if points < 1 {
            return Decision::Check;
        }
        let entry = self.entry(&principle.name, tool);
        if entry.pass_streak < points as u32 {
            return Decision::Check;
        }
        if entry.checks_since_run + 1 >= RECHECK_INTERVAL {
            entry.checks_since_run = 0;
            Decision::Check
        } else {
            entry.checks_since_run += 1;
            Decision::Skip { streak: entry.pass_streak }
        }
    }

    pub fn record(&mut self, principle: &str, tool: &str, passed: bool) {
        let entry = self.entry(principle, tool);
        if passed {
            entry.pass_streak += 1;
        } else {
            entry.pass_streak = 0;
            entry.checks_since_run = 0;
        }
    }
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILENAME)
}

pub fn load(state_dir: &Path) -> RecencyState {
    let p = path(state_dir);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RecencyState::default(),
        Err(e) => {
            warn!(error = %e, path = %p.display(), "reviewed recency: read failed; starting empty");
            return RecencyState::default();
        }
    };
    let records: Vec<Record> = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "reviewed recency: parse failed; starting empty");
            return RecencyState::default();
        }
    };
    let entries = records
        .into_iter()
        .map(|r| {
            (
                (r.principle, r.tool),
                Entry { pass_streak: r.pass_streak, checks_since_run: r.checks_since_run },
            )
        })
        .collect();
    RecencyState { entries }
}

pub fn save(state_dir: &Path, state: &RecencyState) {
    let mut records: Vec<Record> = state
        .entries
        .iter()
        .map(|((principle, tool), e)| Record {
            principle: principle.clone(),
            tool: tool.clone(),
            pass_streak: e.pass_streak,
            checks_since_run: e.checks_since_run,
        })
        .collect();
    records.sort_by(|a, b| a.principle.cmp(&b.principle).then_with(|| a.tool.cmp(&b.tool)));
    let body = match serde_json::to_vec_pretty(&records) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "reviewed recency: serialise failed; not saved");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(state_dir) {
        warn!(error = %e, "reviewed recency: create state dir failed; not saved");
        return;
    }
    let tmp = state_dir.join(format!("{FILENAME}.tmp"));
    if let Err(e) = std::fs::write(&tmp, &body) {
        warn!(error = %e, "reviewed recency: write failed; not saved");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path(state_dir)) {
        warn!(error = %e, "reviewed recency: rename failed; not saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principle(name: &str, recency_points: i32) -> Principle {
        Principle {
            name: name.into(),
            title: "t".into(),
            description: "d".into(),
            kind: Default::default(),
            required: false,
            points: None,
            recency_points,
            persona: "reviewer".into(),
            tools: Vec::new(),
            priority: 100,
            applies_to: Vec::new(),
            context: Vec::new(),
            condition: String::new(),
            children: Vec::new(),
        }
    }

    #[test]
    fn always_checks_when_disabled() {
        let mut s = RecencyState::default();
        let p = principle("p", -1);
        for _ in 0..20 {
            assert_eq!(s.decide(&p, "edit"), Decision::Check);
            s.record(&p.name, "edit", true);
        }
    }

    #[test]
    fn trusts_after_threshold_then_periodic() {
        let mut s = RecencyState::default();
        let p = principle("p", 3);
        // First three are checked, building the streak.
        for _ in 0..3 {
            assert_eq!(s.decide(&p, "edit"), Decision::Check);
            s.record(&p.name, "edit", true);
        }
        // Now trusted: four skips then a forced re-check.
        for _ in 0..4 {
            assert_eq!(s.decide(&p, "edit"), Decision::Skip { streak: 3 });
        }
        assert_eq!(s.decide(&p, "edit"), Decision::Check);
        s.record(&p.name, "edit", true);
        assert!(matches!(s.decide(&p, "edit"), Decision::Skip { .. }));
    }

    #[test]
    fn failure_resets_streak() {
        let mut s = RecencyState::default();
        let p = principle("p", 2);
        s.record(&p.name, "edit", true);
        s.record(&p.name, "edit", true);
        assert!(matches!(s.decide(&p, "edit"), Decision::Skip { .. }));
        s.record(&p.name, "edit", false);
        assert_eq!(s.decide(&p, "edit"), Decision::Check);
    }

    #[test]
    fn streak_is_per_tool() {
        let mut s = RecencyState::default();
        let p = principle("p", 1);
        s.record(&p.name, "read", true);
        assert!(matches!(s.decide(&p, "read"), Decision::Skip { .. }));
        assert_eq!(s.decide(&p, "shell"), Decision::Check);
    }
}
