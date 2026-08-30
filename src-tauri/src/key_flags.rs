//! Key 上「用户直接切换的那种开关」的**单字段**原子写入。
//!
//! 目前只有一个调用方：Key 卡片上那个「允许大脑聚合使用」的 checkbox
//! （`KeyCard.tsx`，只在该 Key 已禁用时渲染）。
//!
//! ## 为什么不走 `upsert_key`
//!
//! `upsert_key` 是**整份替换**，而前端传的是「渲染那张卡片那一刻的快照」。它只沿用库里的
//! `health` 与 `cached_balance` 两项运行态（见那边的 ⚠️ 注释），可后端在运行中会写的
//! **配置**字段不止这两个 —— 最确定的一处是 [`Store::set_balance_query_url`]：余额查询
//! 首次探测命中后，会把真正可用的端点模板写回 `balance_query.url`（那条修复的来由是
//! 「su2api 全查回 10000 USD」）。用户此刻勾一下 checkbox，整份 upsert 就把刚解析出来的
//! 端点顶回旧值。
//!
//! 这条竞态**可自愈**（下次查余额重跑一遍三条探测链），所以它本身不严重。真正的理由是
//! **方向**：一个只翻一个布尔位的操作没有理由携带二十多个字段的旧快照，而「后端又多了一个
//! 自己维护的配置字段」这件事一定会再发生 —— 下一次不保证可自愈，且失效是**静默的**
//! （用户看不出某个字段被顶回了旧值）。
//!
//! 🔴 **不要**改成「把 `allow_in_aggregate` 列进 `upsert_key` 的运行态沿用清单」：
//! 那个 checkbox 本身就是**唯一**写这个字段的地方，列进去等于把开关变成一个永远写不进去的
//! no-op（比现在这条窄竞态糟得多）。
//!
//! ## 为什么挂在 `store` 下
//!
//! `mutate_and_persist`（「改内存 → 落盘 → 失败即从磁盘对账回滚」的唯一写入路径）是
//! **私有**的，只有 `store` 模块的后代能调它。裸 `self.persist()` 在本仓被棘轮计数盯着
//! （落盘失败即「内存领先磁盘」，那个方向永不自愈）。挂载理由同 `log_rotate` / `data_dir`。

use super::Store;
use crate::error::{AppError, AppResult};

/// 只改 `allow_in_aggregate` 这一位，其余字段一个都不碰。
///
/// Key 不存在时返 `NotFound` 而**不是**静默成功：卡片可能握着一份已被删掉的 Key 的快照，
/// 那时静默成功的表现是「勾上了、刷新后自己弹回去」，而用户拿不到任何线索。
pub(crate) fn set_allow_in_aggregate(store: &Store, key_id: &str, allow: bool) -> AppResult<()> {
    store.mutate_and_persist(|cfg| match cfg.keys.iter_mut().find(|k| k.id == key_id) {
        Some(k) => {
            k.allow_in_aggregate = allow;
            Ok(())
        }
        None => Err(AppError::NotFound(key_id.into())),
    })
}

/// Key 卡片上的「允许大脑聚合使用」。命令注册在 `lib.rs` 的 `generate_handler!`。
#[tauri::command]
pub fn set_key_allow_in_aggregate(
    state: tauri::State<crate::AppState>,
    key_id: String,
    allow: bool,
) -> AppResult<()> {
    set_allow_in_aggregate(&state.store, &key_id, allow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::path::PathBuf;

    /// 唯一临时目录（同 `store::tests::temp_dir`：pid + 进程内自增，纳秒粒度只有 100ns，
    /// 单靠时间戳并发下会撞名 —— 那条坑记在 `ccswitch::db_copy_path`）。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("synaroute_test_{}_{}_{}", tag, std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_with_key() -> (Store, ProviderKey) {
        let dir = temp_dir("key_flags");
        let store = Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap();
        let key = ProviderKey {
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            name: "测试Key".into(),
            base_url: "https://api.example.com".into(),
            protocol: Protocol::Anthropic,
            enabled: false,
            balance_query: Some(BalanceQuery {
                enabled: true,
                // 空 url = 「还没探测过」，正是竞态的起点。
                url: String::new(),
                ..Default::default()
            }),
            ..Default::default()
        };
        store.upsert_key(key.clone()).unwrap();
        (store, key)
    }

    fn url_of(store: &Store, id: &str) -> String {
        store
            .list_keys(CategoryType::ClaudeCli)
            .into_iter()
            .find(|k| k.id == id)
            .and_then(|k| k.balance_query.map(|b| b.url))
            .unwrap_or_default()
    }

    fn flag_of(store: &Store, id: &str) -> bool {
        store
            .list_keys(CategoryType::ClaudeCli)
            .into_iter()
            .find(|k| k.id == id)
            .map(|k| k.allow_in_aggregate)
            .unwrap_or(false)
    }

    /// 本模块存在的全部理由，两条路径**并排**跑一遍：
    /// 专用写入只翻那一位；整份 upsert 会把后端刚写回的端点顶掉。
    ///
    /// 对照那一半刻意留着 —— 它就是「为什么不走 upsert_key」的可执行证据。
    /// 哪天 `upsert_key` 真的开始保住这个字段了，它会变红，那时才该重新讨论这个模块。
    #[test]
    fn the_dedicated_write_keeps_what_the_backend_just_wrote_back() {
        let (store, snapshot) = store_with_key();

        // 后端在用户看着卡片的这段时间里解析出了真正可用的余额端点。
        let probed = "{{baseUrl}}/v1/usage";
        assert!(store.set_balance_query_url("k1", probed).unwrap());
        assert_eq!(url_of(&store, "k1"), probed);

        // 专用写入：只翻那一位。
        set_allow_in_aggregate(&store, "k1", true).unwrap();
        assert!(flag_of(&store, "k1"), "开关必须真的写进去");
        assert_eq!(url_of(&store, "k1"), probed, "专用写入不许碰后端自管的配置字段");

        // 对照：卡片握着的旧快照走整份 upsert，端点被顶回空。
        store
            .upsert_key(ProviderKey {
                allow_in_aggregate: true,
                ..snapshot
            })
            .unwrap();
        assert_eq!(
            url_of(&store, "k1"),
            "",
            "这就是本模块要避开的那件事：整份 upsert 用旧快照顶掉了刚探测到的端点。\
             若这条断言变红，说明 upsert_key 已经保住了这个字段 —— 那时再讨论本模块的去留"
        );
    }

    /// 已删掉的 Key：必须报错，不许静默成功（理由见 `set_allow_in_aggregate` 的文档）。
    #[test]
    fn a_vanished_key_is_reported_not_silently_accepted() {
        let (store, _) = store_with_key();
        let err = set_allow_in_aggregate(&store, "不存在的id", true).unwrap_err();
        assert!(
            matches!(err, AppError::NotFound(_)),
            "应报 NotFound，实际 {err:?}"
        );
        // 并且不许顺带改了别人。
        assert!(!flag_of(&store, "k1"));
    }

    /// 🔴 **接线判据**：上面两条都直调函数，把 `KeyCard.tsx` 那一行改回 `api.upsertKey`
    /// 它们照样全绿 —— 而那正是缺陷本身。前端那一侧由
    /// `tests/allowInAggregateWrite.test.ts` 钉住（要读 `.tsx`，跨语言这条缝在 Rust 侧
    /// 反查不到），这里只钉 Rust 侧：命令必须被注册，否则前端调用永远 404。
    ///
    /// 策略门 `invoke-command-must-exist` 查的是「前端调的名字在 Rust 侧存在」，
    /// 反方向（Rust 有 `#[tauri::command]` 但忘了进 `generate_handler!`）它管不到，
    /// 而漏注册的表现是运行时才炸的静默失效。
    #[test]
    fn the_command_must_be_registered_in_the_handler_list() {
        let lib = include_str!("lib.rs");
        assert!(
            lib.contains("key_flags::set_key_allow_in_aggregate"),
            "命令没进 generate_handler!：前端一点那个 checkbox 就是一句 not found"
        );
    }
}
