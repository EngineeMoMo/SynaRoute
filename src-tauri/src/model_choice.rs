//! 「这次请求到底用哪个对外模型名」——客户端发来的，还是应用内选定的。
//!
//! 抽出来是因为 `proxy.rs` 棘轮余量为 0，而这件事在 Codex 模型目录上线后从
//! **一行 `unwrap_or`** 变成了一条需要分类分支的判据。
//!
//! # 🔴 为什么不能继续无条件覆盖
//!
//! 旧实现是 `store.active_model_of(category).unwrap_or(client_model)` —— 应用内选了什么，
//! 就把客户端发来的名字整个换掉。那在「客户端的模型菜单是内置固定清单、拉不到中转的真实
//! 模型」这个前提下是对的（`AppSettings::active_models` 的注释记的正是这个前提）。
//!
//! **那个前提对 Codex 已经不成立**：[`crate::tools::codex::codex_catalog`] 会把可服务模型
//! 写进 Codex 自己的模型目录，用户在 Codex 菜单里**主动选**的名字会如实发过来。
//! 继续无条件覆盖的表现是：选择器做出来了、选了不算 —— 而这是静默的，日志里那条请求
//! 显示的是覆盖后的名字，没有任何线索指向「你的选择被丢掉了」。
//!
//! # 🔴 为什么只对 Codex 改，另两个分类保持「强制」语义
//!
//! 两类客户端发模型名的**动机根本不同**：
//!
//! | 分类 | 谁决定发什么名字 | `active_models` 的正确语义 |
//! |---|---|---|
//! | Codex | **用户在菜单里选** | 兜底（客户端没得选 / 选了个我们服务不了的才生效） |
//! | Claude CLI / 桌面端 | **客户端按任务自动发**（haiku 干杂活、opus 干重活） | 强制（用户就是要把所有请求钉到某个模型上） |
//!
//! Claude Code 那套家族名不是用户的选择，是它自己的调度结果 —— 三档映射
//! （`tier_haiku`/`tier_sonnet`/`tier_opus`）存在的全部理由就是这个。在那里改成
//! 「可服务就尊重」等于把 `active_models` 变成死字段：CLI 发的 haiku/sonnet/opus 几乎总是
//! 可服务的（三档配了就算），于是覆盖永不发生，而用户设的那个「全部走 opus」静默失效。
//!
//! # 效率
//!
//! 第一步就是 `active_model_of`，未配置直接返回 —— 绝大多数用户走这条，零额外成本。
//! 只有配过应用内选模型的 Codex 用户才会多做一次 `enabled_keys_sorted` + 交集。

use crate::model::CategoryType;
use crate::store::Store;

/// 本次请求应当使用的对外模型名。
pub(super) fn pick(store: &Store, category: CategoryType, client_model: String) -> String {
    let Some(active) = store.active_model_of(category) else {
        // 没在应用内选过 → 一律透传客户端原值（现状，也是绝大多数用户的路径）。
        return client_model;
    };
    if category != CategoryType::Codex {
        // 见模块头那张表：CLI / 桌面端的名字是客户端自己调度出来的，不是用户的选择。
        return active;
    }
    let asked = client_model.trim();
    if asked.is_empty() {
        return active;
    }
    // 与应用内选的是同一个 → 结论一样，省掉下面那次全 Key 克隆 + 交集计算。
    // 这不只是优化：`enabled_keys_sorted` 会克隆每条 `ProviderKey`（含 models / mappings /
    // health），而本函数在**转发热路径**上每请求跑一次。本仓整治过同一类开销
    //（「每请求克隆整份 AppSettings 3~4 次」）。
    if asked == active {
        return active;
    }
    // Codex 发来的名字如果我们能服务，那就是用户在它菜单里选的 —— 尊重它。
    // 口径与写进模型目录的那份**同源**（`discoverable_models`，多 Key 取交集），
    // 否则会出现「目录里列了、这里却认不出」的自相矛盾。
    if crate::proxy::discoverable_models(&store.enabled_keys_sorted(category))
        .iter()
        .any(|m| m == asked)
    {
        // 返回 trim 后的值而不是原串：判据用的是 trim 后的形态，返回原串会让下游
        // `resolve_model` 拿到一个与判据不同的字符串（带空白的名字认不出来）。
        return asked.to_string();
    }
    // 认不出：客户端还没重启、仍发着内置的 GPT 名（`gpt-5.6-terra` 之类），
    // 或用户手改过 config.toml。这正是 `active_models` 仍然有价值的那个场景。
    active
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelInfo, ProviderKey};

    fn store_with(category: CategoryType, models: &[&str], active: Option<&str>) -> Store {
        // 进程内自增序号是必须的：纳秒时间戳本机量化粒度只有 100ns，同进程并发跑的
        // 几条用例会撞到同一个目录（同 CLAUDE.md 里 `db_copy_path` 那条）。
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sr_pick_{}_{n}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let key = ProviderKey {
            id: "k1".into(),
            category_id: category,
            enabled: true,
            models: models
                .iter()
                .map(|m| ModelInfo {
                    real_name: (*m).to_string(),
                    source: "manual".into(),
                    fetched_at: None,
                    context_window: None,
                    max_output_tokens: None,
                })
                .collect(),
            ..Default::default()
        };
        store.upsert_key(key).unwrap();
        if let Some(a) = active {
            store.set_active_model(category, a).unwrap();
        }
        store
    }

    /// 🔴 本次修复的本体：用户在 Codex 菜单里选的模型不许被应用内选项吃掉。
    ///
    /// 没有这一条的表现是「模型选择器做出来了、选了不算」，而且是静默的 ——
    /// 日志里那条请求显示的是覆盖后的名字。
    #[test]
    fn a_model_the_user_picked_in_codex_wins_over_the_in_app_choice() {
        let store = store_with(
            CategoryType::Codex,
            &["claude-opus-4-8", "glm-4.6"],
            Some("glm-4.6"),
        );
        assert_eq!(
            pick(&store, CategoryType::Codex, "claude-opus-4-8".into()),
            "claude-opus-4-8",
            "Codex 发来的是可服务模型 → 那是用户在它菜单里选的，必须尊重"
        );
        // 返回值必须是**判据用的那个形态**（trim 后）。返回原串会让下游 `resolve_model`
        // 拿到一个与判据不同的字符串 —— 判得出、却路由不到。
        assert_eq!(
            pick(&store, CategoryType::Codex, "  claude-opus-4-8\n".into()),
            "claude-opus-4-8",
            "带空白的名字要归一后返回，不能原样透传"
        );
    }

    /// 客户端发了个我们服务不了的名字（典型：接入后还没重启 Codex，仍发内置的 GPT 名）
    /// → 这正是应用内选项仍然有价值的场景。
    #[test]
    fn an_unserviceable_name_still_falls_back_to_the_in_app_choice() {
        let store = store_with(CategoryType::Codex, &["glm-4.6"], Some("glm-4.6"));
        assert_eq!(
            pick(&store, CategoryType::Codex, "gpt-5.6-terra".into()),
            "glm-4.6"
        );
        assert_eq!(pick(&store, CategoryType::Codex, "".into()), "glm-4.6");
        assert_eq!(pick(&store, CategoryType::Codex, "   ".into()), "glm-4.6");
    }

    /// 🔴 CLI / 桌面端**保持强制语义**。
    ///
    /// 它们发的 haiku/sonnet/opus 是客户端按任务自动挑的，不是用户的选择。
    /// 在这里也改成「可服务就尊重」会让 `active_models` 变成死字段 ——
    /// 用户设的「全部走 opus」静默失效。
    #[test]
    fn cli_and_desktop_keep_the_forcing_semantics() {
        for cat in [CategoryType::ClaudeCli, CategoryType::ClaudeDesktop] {
            let store = store_with(cat, &["claude-opus-4-8", "claude-haiku-4-5"], Some("claude-opus-4-8"));
            assert_eq!(
                pick(&store, cat, "claude-haiku-4-5".into()),
                "claude-opus-4-8",
                "{cat:?}：客户端自己调度出的家族名不该压过用户设的强制项"
            );
        }
    }

    /// 没在应用内选过 → 一律透传，且不该为此去算可服务集合（热路径上的短路）。
    #[test]
    fn without_an_in_app_choice_everything_passes_through() {
        for cat in [
            CategoryType::Codex,
            CategoryType::ClaudeCli,
            CategoryType::ClaudeDesktop,
        ] {
            let store = store_with(cat, &["m"], None);
            assert_eq!(pick(&store, cat, "whatever-the-client-sent".into()), "whatever-the-client-sent");
        }
    }

    /// 🔴 第 7 次盯同一类接线盲区（前六：`mcp::handle_http` / `route_meta` /
    /// `lan_guard` 的 peer / `log_rotate` 的写线程 / `custom_headers` 的保存 payload /
    /// `tools::apply` 的 models+keys）。
    ///
    /// 上面四条都直接调 `pick`，所以**把 `proxy.rs` 那行改回
    /// `store.active_model_of(category).unwrap_or(client_model)` 它们照样全绿** ——
    /// 而那就是本次修复要消除的缺陷本身（用户在 Codex 菜单里选的模型被静默换掉）。
    #[test]
    fn the_forwarding_path_must_go_through_pick() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("model_choice::pick("),
            "proxy.rs 的生产段必须经由 model_choice::pick 决定对外模型名"
        );
        assert!(
            !prod.contains("active_model_of(category).unwrap_or"),
            "不许绕过 pick 直接无条件覆盖 —— 那正是本次修掉的缺陷"
        );
    }
}
