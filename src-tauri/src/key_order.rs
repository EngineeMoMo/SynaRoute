//! 同分类内 Key 顺序（`priority`）的**唯一**变换实现。
//!
//! 三个入口共用这里的一套步骤：**按当前 priority 稳定排序 → 变换 id 序列 → 只改
//! `priority`、连续重编号为 `0..n-1` → 一次原子落盘**。
//!
//! | 入口 | 语义 | 触发者 |
//! |---|---|---|
//! | [`set_primary`] | 提到首位 | 界面「设为主」、命令面板、托盘「主 Key」子菜单 |
//! | [`move_one`] | 与相邻项交换 | Key 卡片的上移/下移按钮（键盘可达） |
//! | [`reorder_before`] | 放到某条之前 / 末尾 | **鼠标拖放**（2026-09-09 新增） |
//!
//! # 🔴 为什么拖放要新增一个入口，而不是循环调 `move_one`
//!
//! 用户原话是「一个一个点太麻烦了」，也就是说典型动作是**跨多位**移动。而
//! `move_one` 是相邻交换：把第 12 条拖到首位 = 11 次调用 = **11 次整份 config 落盘 +
//! 11 次客户端配置重写（`client_resync`）+ 11 次托盘重建**。后两者都会碰用户的
//! `~/.claude/settings.json`、`~/.codex/config.toml`，且中途任何一次失败都留下**半套顺序**。
//! 一次拖放是一个原子意图，落盘也必须是一次。
//!
//! # 🔴 锚点用 id，不用目标下标
//!
//! `reorder_before(cat, key, before)` 的语义是「把 `key` 放到 `before` 之前；`before` 为
//! `None` 则放末尾」。下标形态（「移到第 3 位」）在**前端快照过期**时会静默错位 ——
//! 拖动期间另一个来源（托盘设主、别的窗口、`reload_if_disk_newer` 自愈）改过顺序，
//! 下标 3 指向的就不是用户看到的那一条了；而锚点 id 在这种情况下仍然表达同一个意图。
//! 前端也**不回传整列 id**：那等于让前端决定顺序，且一条陈旧快照会把并发的改动整片抹掉。
//!
//! # 边界（与既有两个入口完全一致，别在拖放上另立规矩）
//!
//! - **只动同分类**，别的分类一个字节都不碰（有测试钉住）。
//! - **禁用的 Key 也参与重编号**：它在列表里占一个位次，跳过它会让优先级出现空洞。
//! - **只写 `priority`**：正在生效的熔断、余额缓存等运行态一个都不许被顺带覆盖
//!   （旧前端实现走整份 `upsert_key`，那正是它的问题之一）。
//! - **未知 / 跨分类 id 一律 `NotFound`**，不静默改错一条。
//! - 顺序**没有变化**时返回 `false` 且不写盘：拖回原处、拖到自己身上都属正常操作，
//!   不该产生落盘噪音与客户端配置重写。
//!
//! # 为什么挂在 `store` 下
//!
//! `mutate_and_persist`（「改内存 → 落盘 → 失败即从磁盘对账回滚」的唯一写入路径）是
//! **私有**的，只有 `store` 模块的后代调得到。裸 `self.persist()` 被棘轮计数盯着
//! （落盘失败即「内存领先磁盘」，那个方向永不自愈）。挂载理由同 `key_flags` / `log_rotate`。

use super::Store;
use crate::error::{AppError, AppResult};
use crate::model::{CategoryType, ProviderKey};

/// 同分类内按当前 `priority` 升序排好的 id 序列。
///
/// 与界面完全同一口径（`CategoryPage` 也是 `sort((a,b) => a.priority - b.priority)`），
/// **含禁用项**。取 id 而不是整个 `ProviderKey`：后面只需要顺序，克隆整条 Key
/// （含 `models`/`mappings` 两个 Vec）纯属浪费。
fn ordered_ids(cfg: &crate::model::AppConfig, category: CategoryType) -> Vec<String> {
    let mut same: Vec<&ProviderKey> =
        cfg.keys.iter().filter(|k| k.category_id == category).collect();
    same.sort_by_key(|k| k.priority);
    same.iter().map(|k| k.id.clone()).collect()
}

/// 把 `ordered_ids` 表达的顺序落成连续 `priority`，返回**是否真的改了**。
///
/// 重编号成连续值是必须的 —— 历史配置里存在「全部 priority 都是 999」的同级状态，
/// 那时故障转移没有确定的主备顺序（永远先打 `Vec` 里的第一个）。
///
/// 幂等判定放在写之前：目标顺序与现状一致就一个字节都不写。托盘/连点箭头/拖回原处
/// 都会走到这里，无条件落盘等于把 20KB 的 config 反复重写、还带一次客户端配置同步。
fn persist_contiguous(store: &Store, ordered: &[String]) -> AppResult<bool> {
    let changed = {
        let cfg = store.config.read();
        ordered.iter().enumerate().any(|(i, id)| {
            cfg.keys
                .iter()
                .find(|k| &k.id == id)
                .is_some_and(|k| k.priority != i as i32)
        })
    };
    if !changed {
        return Ok(false);
    }
    store.mutate_and_persist(|cfg| {
        for (i, id) in ordered.iter().enumerate() {
            if let Some(k) = cfg.keys.iter_mut().find(|k| &k.id == id) {
                // 只碰 priority。运行态（health / cached_balance）与配置字段一个都不动。
                k.priority = i as i32;
            }
        }
        Ok(())
    })?;
    Ok(true)
}

/// 该分类下必须存在这条 Key，否则 `NotFound`。
///
/// 跨分类误传也算不存在：托盘/命令面板传错分类时，静默改掉**另一个**分类的顺序
/// 比报错糟得多（用户看不出哪里变了）。
fn not_found(category: CategoryType, key_id: &str) -> AppError {
    AppError::NotFound(format!(
        "分类 {} 下没有 id={key_id} 的 Key",
        category.as_str()
    ))
}

fn require_member(cfg: &crate::model::AppConfig, category: CategoryType, key_id: &str) -> AppResult<()> {
    if cfg
        .keys
        .iter()
        .any(|k| k.id == key_id && k.category_id == category)
    {
        return Ok(());
    }
    Err(not_found(category, key_id))
}

/// 把某 Key 提为该分类的「主 Key」（优先级 0），其余保持原相对顺序顺延。
pub(crate) fn set_primary(store: &Store, category: CategoryType, key_id: &str) -> AppResult<bool> {
    let ordered = {
        let cfg = store.config.read();
        require_member(&cfg, category, key_id)?;
        let mut ids = ordered_ids(&cfg, category);
        if let Some(pos) = ids.iter().position(|id| id == key_id) {
            let target = ids.remove(pos);
            ids.insert(0, target);
        }
        ids
    };
    persist_contiguous(store, &ordered)
}

/// 把某 Key 在同分类内上移/下移一位。已在两端时返回 `false`。
pub(crate) fn move_one(
    store: &Store,
    category: CategoryType,
    key_id: &str,
    up: bool,
) -> AppResult<bool> {
    let ordered = {
        let cfg = store.config.read();
        require_member(&cfg, category, key_id)?;
        let mut ids = ordered_ids(&cfg, category);
        // 上一行已经保证它在这个分类里；找不到就是并发删了，按 NotFound 走。
        let idx = ids
            .iter()
            .position(|id| id == key_id)
            .ok_or_else(|| not_found(category, key_id))?;
        let swap_with = if up {
            if idx == 0 {
                return Ok(false); // 已在首位
            }
            idx - 1
        } else {
            if idx + 1 >= ids.len() {
                return Ok(false); // 已在末位
            }
            idx + 1
        };
        ids.swap(idx, swap_with);
        ids
    };
    persist_contiguous(store, &ordered)
}

/// 把 `key_id` 移到 `before_key_id` **之前**；`before_key_id` 为 `None` 则移到末尾。
///
/// 这是鼠标拖放的落点。先从序列里摘掉 source 再插入，故「往下拖」时锚点的位置会自然
/// 前移一格 —— 这正是用户看到的插入线语义（线画在锚点上方）。
///
/// 🔴 **`before == key_id` 返回 `false` 而不是报错**：那是「拖到自己身上」，
/// 是完全正常的手滑，报错会弹一个 toast 说「无效参数」，而用户什么都没做错。
pub(crate) fn reorder_before(
    store: &Store,
    category: CategoryType,
    key_id: &str,
    before_key_id: Option<&str>,
) -> AppResult<bool> {
    let ordered = {
        let cfg = store.config.read();
        require_member(&cfg, category, key_id)?;
        // 锚点也必须是同分类的成员。跨分类锚点若被放过，`position` 会找不到它、
        // 静默退化成「移到末尾」—— 一个用户没有表达过的意图。
        if let Some(anchor) = before_key_id {
            if anchor == key_id {
                return Ok(false);
            }
            require_member(&cfg, category, anchor)?;
        }
        let mut ids = ordered_ids(&cfg, category);
        // ⚠️ **这一支今天不可达**，`require_member` 与 `ordered_ids` 读的是同一个读锁下的
        // 同一份 `cfg`，前者过了就必然找得到。**没有测试守着它**（注入 `.unwrap()`
        // 实测仍绿 —— 6 条用例全过），写成 `?` 只是不想在 IPC 上留一个 panic 点：
        // 日后有人把两处拆到不同临界区，失效方向就从「返回错误」变成「panic」。
        // 别把这句读成「有判据在守」。
        let from = ids
            .iter()
            .position(|id| id == key_id)
            .ok_or_else(|| not_found(category, key_id))?;
        let moved = ids.remove(from);
        match before_key_id {
            // 摘掉 source **之后**再找锚点位置：先找会在「往下拖」时偏一格。
            Some(anchor) => {
                let at = ids
                    .iter()
                    .position(|id| id == anchor)
                    .unwrap_or(ids.len());
                ids.insert(at, moved);
            }
            None => ids.push(moved),
        }
        ids
    };
    persist_contiguous(store, &ordered)
}

/// **鼠标拖放**重排的 IPC 命令：把 `key_id` 放到 `before_key_id` 之前（`None` = 末尾）。
///
/// 与 `move_key` 并列而不是替代它 —— 上下移按钮是键盘/无障碍路径，必须留着
/// （拖放在辅助技术下不可达）。两者共用本模块的同一套变换。
///
/// 命令放在这里而不是 `lib.rs`：那个文件的棘轮余量是 0，且「命令跟着实现走」是本仓
/// 既有做法（同 `key_flags` / `codex_catalog` / `balance_gate`）。注册在
/// `generate_handler!` 里写全路径。
///
/// 收尾与 `move_key` 逐字一致，两步都不能省：
/// - **`client_resync::sync_after`**：拖到首位换掉的是**客户端的默认模型**
///   （CLI 的 `ANTHROPIC_MODEL`、Codex 目录的 `isDefault`、桌面端 `inferenceModels` 顺序
///   都跟着首位走）。不刷的表现是「界面说换了、客户端没换」。
/// - **变了才 `rebuild_tray`**：主 Key 可能因此易主，托盘勾选要跟上；没变时不刷，
///   避免把一次 no-op 变成托盘菜单重建。
#[tauri::command]
pub fn reorder_key(
    app: tauri::AppHandle,
    state: tauri::State<crate::AppState>,
    category_id: CategoryType,
    key_id: String,
    before_key_id: Option<String>,
) -> AppResult<bool> {
    let changed = crate::client_resync::sync_after(
        &state,
        state
            .store
            .reorder_key(category_id, &key_id, before_key_id.as_deref()),
    )?;
    if changed {
        let _ = crate::rebuild_tray(&app);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CategoryType;
    use crate::service::tests::{key, temp_store};

    /// 造一条指定 id / priority 的 Key（分类默认 ClaudeCli）。
    fn seed(store: &Store, id: &str, priority: i32) {
        let mut k = key(CategoryType::ClaudeCli);
        k.id = id.into();
        k.priority = priority;
        store.upsert_key(k).unwrap();
    }

    /// 当前顺序（含禁用项），按 priority 升序。
    fn order(store: &Store, category: CategoryType) -> Vec<String> {
        let mut all = store.list_keys(category);
        all.sort_by_key(|k| k.priority);
        all.into_iter().map(|k| k.id).collect()
    }

    /// 优先级必须始终是连续的 `0..n-1`（同级下故障转移没有确定的主备顺序）。
    fn assert_contiguous(store: &Store, category: CategoryType) {
        let mut all = store.list_keys(category);
        all.sort_by_key(|k| k.priority);
        let prios: Vec<i32> = all.iter().map(|k| k.priority).collect();
        assert_eq!(
            prios,
            (0..prios.len() as i32).collect::<Vec<_>>(),
            "priority 必须连续，实际 {prios:?}"
        );
    }

    /// 🔴 拖放的核心用例：**跨多位**移动一次就到位（用户原话「一个一个点太麻烦了」）。
    ///
    /// 故障注入判据：把 `reorder_before` 的插入位置改成「摘除前算出的下标」→
    /// 往下拖那一段变红（会偏一格）。
    #[test]
    fn dragging_across_many_positions_lands_exactly_where_asked() {
        let (store, dir) = temp_store("reorder_far");
        for (i, id) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            seed(&store, id, i as i32);
        }

        // 把末尾的 e 拖到最前（锚点 = a）。
        assert!(reorder_before(&store, CategoryType::ClaudeCli, "e", Some("a")).unwrap());
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["e", "a", "b", "c", "d"]);
        assert_contiguous(&store, CategoryType::ClaudeCli);

        // 往**下**拖：把 e 放到 d 之前 → e 落在 c 与 d 之间。
        // 这一段是「摘除后再定位」的判据：先定位会把它插到 c 之前。
        assert!(reorder_before(&store, CategoryType::ClaudeCli, "e", Some("d")).unwrap());
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["a", "b", "c", "e", "d"]);

        // 锚点 None = 放到末尾。
        assert!(reorder_before(&store, CategoryType::ClaudeCli, "a", None).unwrap());
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["b", "c", "e", "d", "a"]);
        assert_contiguous(&store, CategoryType::ClaudeCli);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 没有实际变化的三种拖放都必须返回 `false` 且**不写盘**。
    ///
    /// 为什么要钉「不写盘」：每次落盘都会连带一次 `client_resync`（重写用户的
    /// `~/.claude/settings.json` / `~/.codex/config.toml`）与托盘重建。拖回原处是常见手滑，
    /// 让它产生这一串副作用毫无道理。
    #[test]
    fn a_drag_that_changes_nothing_does_not_touch_the_disk() {
        let (store, dir) = temp_store("reorder_noop");
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            seed(&store, id, i as i32);
        }
        let cfg_path = store.config_path_display();
        let before = std::fs::read(&cfg_path).unwrap();

        // ① 拖到自己身上（锚点 == source）：正常手滑，不该报错。
        assert!(!reorder_before(&store, CategoryType::ClaudeCli, "b", Some("b")).unwrap());
        // ② 拖到原来的后继之前 = 原地不动。
        assert!(!reorder_before(&store, CategoryType::ClaudeCli, "a", Some("b")).unwrap());
        // ③ 末尾那条再拖到末尾。
        assert!(!reorder_before(&store, CategoryType::ClaudeCli, "c", None).unwrap());

        assert_eq!(std::fs::read(&cfg_path).unwrap(), before, "无变化时磁盘不该被改写");
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["a", "b", "c"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 未知 source / 未知锚点 / 跨分类锚点都必须报错，不许静默改成别的顺序。
    #[test]
    fn unknown_or_cross_category_ids_are_rejected() {
        let (store, dir) = temp_store("reorder_reject");
        seed(&store, "a", 0);
        seed(&store, "b", 1);
        let mut cx = key(CategoryType::Codex);
        cx.id = "cx".into();
        cx.priority = 0;
        store.upsert_key(cx).unwrap();

        assert!(
            reorder_before(&store, CategoryType::ClaudeCli, "nope", Some("a")).is_err(),
            "source 不存在"
        );
        assert!(
            reorder_before(&store, CategoryType::ClaudeCli, "a", Some("nope")).is_err(),
            "锚点不存在"
        );
        assert!(
            reorder_before(&store, CategoryType::ClaudeCli, "a", Some("cx")).is_err(),
            "🔴 跨分类锚点必须拒绝 —— 放过它会静默退化成「移到末尾」，\
             那是用户没有表达过的意图"
        );
        assert!(
            reorder_before(&store, CategoryType::Codex, "a", None).is_err(),
            "id 存在但分类不符也算不存在"
        );
        // 拒绝之后顺序一个都没动。
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["a", "b"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 拖放**只碰同分类**，且**只写 priority**（运行态不许被顺带覆盖）。
    ///
    /// 后半段与 `move_key_swaps_renumbers_and_preserves_health` 同一条纪律：旧前端实现走
    /// 整份 `upsert_key`，会把打开页面那一刻的 health 快照写回去、静默解除正在生效的熔断。
    #[test]
    fn reordering_touches_only_priority_and_only_this_category() {
        let (store, dir) = temp_store("reorder_scope");
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            seed(&store, id, i as i32);
        }
        // 别的分类：优先级刻意留成不连续的 5 / 7，验证它不被牵连重编号。
        for (id, p) in [("cx_1", 5), ("cx_2", 7)] {
            let mut k = key(CategoryType::Codex);
            k.id = id.into();
            k.priority = p;
            store.upsert_key(k).unwrap();
        }
        // b 连续失败到武装熔断（分类页此时显示「熔断中」横幅）。
        for _ in 0..3 {
            crate::health::record_live_failure(&store, "b");
        }
        assert!(
            store.get_key("b").unwrap().health.breaker_until.is_some(),
            "前置条件：b 应已武装熔断"
        );

        assert!(reorder_before(&store, CategoryType::ClaudeCli, "c", Some("a")).unwrap());

        assert_eq!(order(&store, CategoryType::ClaudeCli), ["c", "a", "b"]);
        assert!(
            store.get_key("b").unwrap().health.breaker_until.is_some(),
            "调整顺序不得解除正在生效的熔断"
        );
        assert_eq!(store.get_key("cx_1").unwrap().priority, 5, "跨分类不得被牵连重编号");
        assert_eq!(store.get_key("cx_2").unwrap().priority, 7, "跨分类不得被牵连重编号");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 禁用的 Key 也参与拖放与重编号 —— 它在列表里占一个位次。
    ///
    /// 跳过它的表现是优先级出现空洞/重复，而那会让故障转移在同级之间失去确定顺序。
    /// 另一半同样要成立：**首位是禁用项时，「路由意义上的主 Key」仍是首个启用项**
    /// （`enabled_keys_sorted` 的口径，界面徽标与托盘都用它）。
    #[test]
    fn disabled_keys_take_part_in_ordering_but_never_become_the_routing_primary() {
        let (store, dir) = temp_store("reorder_disabled");
        seed(&store, "a", 0);
        let mut off = key(CategoryType::ClaudeCli);
        off.id = "off".into();
        off.priority = 1;
        off.enabled = false;
        store.upsert_key(off).unwrap();
        seed(&store, "c", 2);

        // 把禁用的那条拖到最前。
        assert!(reorder_before(&store, CategoryType::ClaudeCli, "off", Some("a")).unwrap());
        assert_eq!(order(&store, CategoryType::ClaudeCli), ["off", "a", "c"]);
        assert_contiguous(&store, CategoryType::ClaudeCli);

        assert_eq!(
            store
                .enabled_keys_sorted(CategoryType::ClaudeCli)
                .first()
                .map(|k| k.id.clone()),
            Some("a".to_string()),
            "首位是禁用项时，主 Key 仍是首个**启用**的那条"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 三个入口共用同一套变换：拖到首位 ≡ 设为主；拖到后继之前 ≡ 下移一格。
    ///
    /// 钉这条是为了防「拖放另写一套重排规则」—— 那会让「从拖放得到的顺序」与
    /// 「从按钮/托盘得到的顺序」在同一个意图上产出不同结果，而用户无从预期。
    #[test]
    fn the_three_entry_points_agree_on_the_same_intent() {
        let (store, dir) = temp_store("reorder_agree");
        for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
            seed(&store, id, i as i32);
        }

        // 拖到首位 == 设为主
        reorder_before(&store, CategoryType::ClaudeCli, "c", Some("a")).unwrap();
        let via_drag = order(&store, CategoryType::ClaudeCli);
        set_primary(&store, CategoryType::ClaudeCli, "a").unwrap(); // 还原
        set_primary(&store, CategoryType::ClaudeCli, "c").unwrap();
        assert_eq!(via_drag, order(&store, CategoryType::ClaudeCli), "拖到首位必须与设为主一致");

        // 下移一格 == 拖到「后继的**后继**」之前。
        //
        // ⚠️ 这里我第一版写的是「拖到后继之前」，注入时被自己的断言打回：那是**原地不动**
        // （a 本来就紧挨在 b 前面），返回 false。语义上「插到 X 之前」要越过一整条才算换位，
        // 所以锚点得是 snapshot[3]。留着这段说明，免得下一个人照直觉再改回去。
        let snapshot = order(&store, CategoryType::ClaudeCli);
        let anchor = snapshot[3].clone();
        assert!(
            reorder_before(&store, CategoryType::ClaudeCli, &snapshot[1], Some(&anchor)).unwrap(),
            "越过一条应当真的改变顺序"
        );
        let via_drag = order(&store, CategoryType::ClaudeCli);
        // 还原后用 move_one 走一遍同样的意图
        persist_contiguous(&store, &snapshot).unwrap();
        move_one(&store, CategoryType::ClaudeCli, &snapshot[1], false).unwrap();
        assert_eq!(via_drag, order(&store, CategoryType::ClaudeCli), "拖一格必须与下移一格一致");

        std::fs::remove_dir_all(&dir).ok();
    }
}
