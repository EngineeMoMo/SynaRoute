//! Laya only ever raises effort; it never lowers it.
//!
//! Measured on the local multilingual checkpoint (2026-10-20): the three-level complexity question
//! was adopted 7 times in 72 Chinese/English tasks, almost always as `low`, including medium and hard
//! tasks lowered to `low`, and never as a confident `high`. A downgrade costs answer quality; a wrong
//! upgrade only costs time and tokens. So Laya answers one yes/no question, "does this need deep
//! reasoning", and a confident yes raises authorized models to `high`; anything else keeps the
//! original settings.
//!
//! The question and threshold were chosen on 18 calibration tasks, then measured once on 36 held-out
//! tasks: 4 of 12 hard tasks upgraded, 1 of 24 others upgraded wrongly. Jev and OpenAI-compatible
//! judges keep the three-level question.
use serde_json::{json, Value};

pub(super) const QUESTION: &str = "Does answering this task require deep, multi-step reasoning such as system architecture, ambiguous debugging or a formal proof?";
/// Fixed on the calibration set before the held-out set was scored.
pub(super) const THRESHOLD: f64 = 0.75;

pub(super) fn body(model: &str, prompt: &str) -> Value {
    json!({"model": model, "state": prompt, "questions": {"deep": {"type": "noul", "instructions": QUESTION}}})
}

/// A truncated state, a missing score or anything below the threshold is "no".
pub(super) fn needs_deep_reasoning(body: &Value) -> bool {
    let usage = &body["usage"];
    if usage["truncated"] == true
        || usage["truncated"].as_u64().is_some_and(|n| n > 0)
        || usage["state_tokens_dropped"].as_u64().is_some_and(|n| n > 0)
    {
        return false;
    }
    body["answers"]["deep"]["noul"]
        .as_f64()
        .is_some_and(|v| v.is_finite() && v >= THRESHOLD)
}

/// The native effort to use for a deep task: `high`, only when it is strictly above the user's
/// manual choice in the client's own ordering. Never a lower or invented level.
pub(super) fn pick_upgrade(efforts: &[String], manual: &str) -> Option<String> {
    let current = efforts.iter().position(|e| e == manual)?;
    let target = efforts.iter().position(|e| e == "high")?;
    (target > current).then(|| efforts[target].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_confident_untruncated_yes_counts() {
        let answer = |v: f64| json!({"answers": {"deep": {"noul": v}}});
        assert!(needs_deep_reasoning(&answer(THRESHOLD)));
        assert!(!needs_deep_reasoning(&answer(0.74)));
        assert!(!needs_deep_reasoning(&json!({"answers": {}})), "missing score");
        let mut truncated = answer(0.99);
        truncated["usage"] = json!({"truncated": true});
        assert!(!needs_deep_reasoning(&truncated), "a truncated task was not read");
        truncated["usage"] = json!({"state_tokens_dropped": 4});
        assert!(!needs_deep_reasoning(&truncated));
        assert_eq!(body("multilingual", "t")["questions"]["deep"]["type"], "noul");
    }
    #[test]
    fn native_upgrade_never_lowers_or_invents() {
        let codex: Vec<String> = ["low", "medium", "high", "xhigh"].map(String::from).to_vec();
        assert_eq!(pick_upgrade(&codex, "low").as_deref(), Some("high"));
        assert_eq!(pick_upgrade(&codex, "medium").as_deref(), Some("high"));
        assert_eq!(pick_upgrade(&codex, "high"), None, "already high");
        assert_eq!(pick_upgrade(&codex, "xhigh"), None, "never lower");
        assert_eq!(pick_upgrade(&codex, "unlisted"), None);
        assert_eq!(pick_upgrade(&["low".into(), "max".into()], "low"), None, "no high in this catalog");
    }
}
