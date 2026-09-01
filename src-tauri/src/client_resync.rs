//! Key 变化之后，把**当前**可服务模型清单重写进正在跑的那些分类的客户端配置。
//!
//! # 🔴 为什么需要它（2026-08-31 用户实报）
//!
//! 模型清单此前只在三个时刻写进客户端配置：接入、改端口、托盘模型子菜单 ——
//! **新增 / 启停 / 删除 Key、改主 Key 都不在其中**。于是用户加了一条 Key，代理里立刻能
//! 路由到它的模型，而 Codex 菜单与桌面端选择器里一个新模型都看不到，得手动再点一次「接入」。
//!
//! # 三端的「动态」程度完全不同，这是本模块的边界
//!
//! | 客户端 | 模型清单从哪来 | 加了 Key 之后 |
//! |---|---|---|
//! | Claude Code CLI | 每次 `/model` 都 GET `<base>/v1/models`，而那是**每请求现算**的 | **零操作**，立刻可见 |
//! | Claude 桌面端 | gateway 档的 `inferenceModels`（静态）+ 启动那一刻的运行时发现 | 本模块重写文件 → 重启桌面端 |
//! | Codex | `model_catalog_json` 指向的目录文件（官方注明 applied on startup only） | 本模块重写文件 → 重启 Codex |
//!
//! **Codex 那条为什么做不到真动态**：它的两条通道互斥 ——
//! `StaticModelsManager::raw_model_catalog` 把 `_refresh_strategy` 与 `_http_client_factory`
//! 整个丢弃、`refresh_if_new_etag` 是空实现，**配了目录就永不联网**。换走 HTTP `/models`
//! 能拿到 etag 刷新，代价是它会写 `~/.codex/models_cache.json`，而那是**全局单文件、
//! 无 provider 维度**（只有一份 `fetched_at`/`etag`/`client_version`）→「用户切回官方登录后
//! 会不会读到我们的列表」没有任何字段能保证。取舍全文在
//! [`crate::tools::codex::codex_catalog`] 的模块头，本模块**不改那条通道**。
//!
//! 顺带一条容易记反的事实：模型**清单**要重启，而模型**选择**不用 ——
//! `get_default_model` 在每次 session 初始化时读 `config.toml` 的顶层 `model`，新开会话即生效。
//!
//! # 🔴 只对**正在跑**的分类写
//!
//! 没在跑 = 没接入，此时去碰用户的 `~/.codex/config.toml` 或桌面端配置是越界的 ——
//! 与「起=写、停=还原」那条纪律同源（见 [`crate::service::apply_tool_config`] 的文档）。
//! 有源码级判据钉住这一点。
//!
//! # 边界：两条会改写清单、却**没有**过本模块的路径
//!
//! 1. **`move_key`（上移/下移优先级）**。[`crate::proxy::discoverable_models`] 按 Key 顺序
//!    产出，故重排会改清单的**顺序** —— Codex 目录里的 `priority`/`isDefault`、桌面端
//!    `inferenceModels` 的排列都跟着变。**集合不变**，所以影响止于「菜单里的先后」，
//!    下一次任何 Key 编辑就会顺带刷新。同为重排的 `set_primary_key` 过了本模块，
//!    这处不对称是**成本决定的**：`lib.rs` 棘轮余量为 0，而收益是排序美观。
//! 2. **`fetch_models`（拉取并落盘）**。它经 `Store::set_models` 直接改**集合**，
//!    影响是功能性的；今天无害只因为**前端零调用**（唯一入口是编辑器里的「拉取」按钮，
//!    走不落盘的 `fetch_models_draft` + 保存，那条走 `upsert_key` → 本模块）。
//!    有判据 `fetch_models_stays_unreachable_until_someone_wires_the_resync` 钉住这个前提：
//!    谁接上按钮就会变红，逼他一并过这里。
//!
//! # 🔴 失败只记事件，不让 Key 操作跟着失败
//!
//! Key 已经存好了。返回 `Err` 会让前端显示「保存失败」而其实存上了 —— 那比清单陈旧糟得多
//! （用户会反复重试、或以为配置丢了）。同 `service::register_and_record` 的取舍。

use crate::error::AppResult;
use crate::model::CategoryType;

/// 在一次 Key 变更**成功之后**同步客户端配置，并原样透传结果。
///
/// 刻意设计成「包住 `Result`」而不是「调用方各自 `if ok` 再调一次」：四个入口
/// （`upsert_key` / `delete_key` / `toggle_key` / `set_primary_key`）于是都是**一行换一行**，
/// 而「记得在每个出口调一次」是必然会漏的纪律 —— 本仓为这类接线盲区栽过十几次。
/// 有源码级判据钉住这四处都过了这里。
pub(crate) fn sync_after<T>(state: &crate::AppState, result: AppResult<T>) -> AppResult<T> {
    if result.is_ok() {
        resync_running(state);
    }
    result
}

/// 对每个**正在跑**的分类重写一次客户端配置。
///
/// `port_of` 返回 `None` 就跳过 —— 那正是「没接入，别碰用户的文件」这道判据本身。
fn resync_running(state: &crate::AppState) {
    for category in CategoryType::ALL {
        let Some(port) = state.proxy.port_of(category) else {
            continue;
        };
        let keys = state.store.enabled_keys_sorted(category);
        let models = crate::proxy::discoverable_models(&keys);
        let endpoint = format!("http://127.0.0.1:{port}");
        match crate::tools::apply(category, &endpoint, &models, &keys) {
            // 成功时**不记 config 事件**：这是自动同步，每编辑一次 Key 就刷一条是纯噪音，
            // 而 `backup_and_write_bytes` 在「序列化结果与磁盘一致」时压根不写盘 ——
            // 绝大多数情况下什么都没变。桌面端的对外名体检要照做（它只在真有不合规名时落事件），
            // 否则并集之后新冒出来的不合规名会一直静默被桌面端过滤掉。
            Ok(_) => {
                crate::service::warn_desktop_unacceptable_models(&state.store, category, &models)
            }
            Err(e) => state.store.append_event(
                category,
                "error",
                None,
                &format!("Key 变更后重写客户端配置失败（清单可能仍是旧的，可手动点一次接入）: {e}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    /// 剥注释再查（本仓已 5 次栽在「注释里的字面量满足了断言」上）。
    fn prod(src: &str) -> String {
        crate::proxy::custom_headers::production_code_only(src)
    }

    /// 🔴 四个 Key 变更入口**都**必须过 [`super::sync_after`]。
    ///
    /// 漏掉任何一个的表现是「改这一种 Key 属性会同步、改那一种不会」—— 比完全不同步更难查，
    /// 因为用户会得到一个时对时错的心智模型。本仓为这类接线盲区栽过十几次，
    /// 每次都是「单元覆盖了组件、没覆盖调用它的那条线」。
    #[test]
    fn every_key_mutation_command_must_go_through_sync_after() {
        let src = prod(include_str!("lib.rs"));
        assert_eq!(
            src.matches("client_resync::sync_after(").count(),
            4,
            "upsert_key / delete_key / toggle_key / set_primary_key 四处都要过它"
        );
    }

    /// 🔴 只对**正在跑**的分类写：没接入就不许碰用户的 `~/.codex/config.toml` 等文件。
    ///
    /// 去掉这道门的表现最坏 —— 用户从没点过接入，我们却在他改一条 Key 时就替他改写了
    /// 客户端配置（同 docs 里记的「导入配置带走 proxy_running_categories」那条缺陷）。
    #[test]
    fn only_running_categories_are_touched() {
        let src = prod(include_str!("client_resync.rs"));
        assert!(
            src.contains("state.proxy.port_of(category)"),
            "必须按 port_of 判定「这个分类是否正在跑」"
        );
    }
}
