//! Codex Desktop 对模型目录的推理档位做的**第二层过滤**。
//!
//! 取证（Codex Desktop 26.901.5280.0 的 `app.asar`）：模型目录先提供
//! `supportedReasoningEfforts`，随后前端只保留 `enabled-reasoning-efforts` 中的值。
//! 其默认列表有 low/medium/high/xhigh/ultra/persistent，唯独没有 max；所以 Luna 即使
//! 从目录拿到五档，菜单仍只显示四档。
//!
//! Ultra 是另一条机制：先受远端 Statsig gate `1186680773` 过滤。本地的
//! `show-ultra-in-model-picker-slider` 只是 ChatGPT 一次性迁移标记，不是 Ultra 总开关。
//! 故本模块只补 max，绝不声称能绕过远端资格门。

const SETTING: &str = "enabled-reasoning-efforts";
const DEFAULT: [&str; 6] = ["low", "medium", "high", "xhigh", "ultra", "persistent"];

/// 在 `[desktop].enabled-reasoning-efforts` 里补上 `max`。
///
/// 保留用户已有顺序与其它值；已有 max 时幂等；`desktop` 或该键被用户写成非预期类型时
/// fail-closed，不替他覆盖。整份 config 仍由接入前 `.bak` 负责正常还原。
pub(super) fn enable_max(table: &mut toml::value::Table) -> bool {
    let desktop = table
        .entry("desktop".to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
    let Some(desktop) = desktop.as_table_mut() else { return false };
    let levels = desktop.entry(SETTING.to_string()).or_insert_with(|| {
        toml::Value::Array(
            DEFAULT.into_iter().map(|s| toml::Value::String(s.to_string())).collect(),
        )
    });
    let Some(levels) = levels.as_array_mut() else { return false };
    if levels.iter().any(|v| v.as_str() == Some("max")) {
        return false;
    }
    levels.push(toml::Value::String("max".into()));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_is_added_without_overwriting_existing_preferences() {
        let mut root = toml::value::Table::new();
        root.insert(
            "desktop".into(),
            toml::Value::Table(toml::toml! {
                enabled-reasoning-efforts = ["high", "low", "custom"]
                other = true
            }),
        );
        assert!(enable_max(&mut root));
        let desktop = root["desktop"].as_table().unwrap();
        let got: Vec<&str> = desktop[SETTING]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(got, ["high", "low", "custom", "max"]);
        assert_eq!(desktop["other"].as_bool(), Some(true));
        assert!(!enable_max(&mut root), "第二次必须幂等");
    }

    #[test]
    fn missing_setting_gets_codex_defaults_plus_max() {
        let mut root = toml::value::Table::new();
        assert!(enable_max(&mut root));
        let got = root["desktop"][SETTING].as_array().unwrap();
        assert_eq!(got.len(), DEFAULT.len() + 1);
        assert!(got.iter().any(|v| v.as_str() == Some("max")));
        assert!(!got.iter().any(|v| v.as_str() == Some("none")));
    }

    #[test]
    fn malformed_user_values_are_not_overwritten() {
        let mut root = toml::toml! { desktop = 1 };
        assert!(!enable_max(&mut root));
        assert_eq!(root["desktop"].as_integer(), Some(1));
    }
}
