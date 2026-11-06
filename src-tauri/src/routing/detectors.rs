use super::config::PrivacyConfig;
use regex::Regex;
use serde::Serialize;
use std::{collections::BTreeMap, sync::OnceLock};

const MAX_MATCHES: usize = 256;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding { pub kind: String, pub start: usize, pub end: usize }
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub counts: BTreeMap<String, usize>,
    pub findings: Vec<Finding>,
    pub redacted: String,
    pub limited: bool,
}

fn patterns() -> &'static Vec<(&'static str, Regex, usize)> {
    static PATTERNS: OnceLock<Vec<(&str, Regex, usize)>> = OnceLock::new();
    PATTERNS.get_or_init(|| [
        ("private_key", r"(?s)-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----.*?(?:-----END (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----|\z)", 0),
        ("api_key", r"\b(?:sk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AIza[A-Za-z0-9_-]{30,}|AKIA[A-Z0-9]{16})\b", 0),
        ("token", r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b", 0),
        ("credential", r#"(?i)\b(?:api[_-]?key|access[_-]?token|auth[_-]?token|refresh[_-]?token|password|passwd|client[_-]?secret)[\"']?\s*[:=]\s*[\"']?([^\s\"',;}]{6,})"#, 1),
        ("email", r"\b[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)+\b", 0),
        ("phone", r"(?:\+86[- ]?)?\b1[3-9][0-9]{9}\b", 0),
    ].into_iter().map(|(kind, pattern, capture)| (kind, Regex::new(pattern).expect("constant detector pattern"), capture)).collect())
}

fn placeholder(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "example" | "changeme" | "your_key" | "your_api_key" | "your_token" | "redacted" | "synaroute-proxy")
        || value.chars().all(|c| c == '*') || value.starts_with("${") || value.starts_with("{{")
}

pub fn scan(text: &str, config: &PrivacyConfig, terms: &[String]) -> Scan {
    let mut result = Scan::default();
    for (kind, pattern, capture) in patterns() {
        let enabled = match *kind { "email" => config.emails, "phone" => config.phones, _ => config.credentials };
        if !enabled { continue; }
        for captures in pattern.captures_iter(text).take(MAX_MATCHES + 1) {
            let Some(value) = captures.get(*capture) else { continue; };
            if *kind == "credential" && placeholder(value.as_str()) { continue; }
            if result.findings.len() == MAX_MATCHES { result.limited = true; break; }
            result.findings.push(Finding { kind: (*kind).into(), start: value.start(), end: value.end() });
        }
    }
    for term in terms.iter().filter(|s| !s.is_empty()) {
        for (start, _) in text.match_indices(term).take(MAX_MATCHES + 1) {
            if result.findings.len() == MAX_MATCHES { result.limited = true; break; }
            result.findings.push(Finding { kind: "custom".into(), start, end: start + term.len() });
        }
    }
    result.findings.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    let mut end = 0;
    result.findings.retain(|f| { if f.start < end { false } else { end = f.end; true } });
    let mut cursor = 0;
    for finding in &result.findings {
        *result.counts.entry(finding.kind.clone()).or_default() += 1;
        result.redacted.push_str(&text[cursor..finding.start]);
        result.redacted.push_str("[REDACTED]");
        cursor = finding.end;
    }
    result.redacted.push_str(&text[cursor..]);
    if result.limited { result.redacted = "[Scan limit reached; preview withheld]".into(); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_credentials_and_custom_unicode_without_returning_values() {
        let token = format!("sk-proj-{}", "x".repeat(30));
        let label = "PRIVATE KEY";
        let raw = format!("hello 密码示例 {token}\npassword='private-password'\n-----BEGIN {label}-----\nsecret\n-----END {label}-----");
        let result = scan(&raw, &PrivacyConfig::default(), &["密码示例".into()]);
        for value in [&token, "private-password", "密码示例", "secret"] { assert!(!result.redacted.contains(value)); }
        assert_eq!(result.counts.len(), 4);
        assert!(!serde_json::to_string(&result.findings).unwrap().contains(&token));
    }
    #[test]
    fn optional_personal_data_and_placeholders_do_not_trigger_by_default() {
        let raw = "user@example.com 13812345678 password=changeme api_key=your_api_key";
        assert!(scan(raw, &PrivacyConfig::default(), &[]).findings.is_empty());
        let config = PrivacyConfig { emails:true, phones:true, ..Default::default() };
        let result = scan(raw, &config, &[]);
        assert_eq!(result.counts.get("email"), Some(&1));
        assert_eq!(result.counts.get("phone"), Some(&1));
    }
}
