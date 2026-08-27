//! 客户端配置文件的**写入 / 备份 / 回滚 / 还原**这一层。
//!
//! 从 `tools.rs` 抽出来的（那边棘轮余量为 0），但也本该独立：这四件事是一套完整的语义，
//! 而 `tools.rs` 其余部分讲的是「往三个客户端各写什么字段」——两种关注点。
//!
//! # 一条贯穿本模块的纪律：`.bak` 是「**接入前**快照」，不是「上次写入前快照」
//!
//! [`backup_and_write_bytes`] 是**首写即锁**：`.bak` 恒为「SynaRoute 第一次动这个文件之前」
//! 的内容，重复接入多少次都不变质；[`restore_one`] 还原成功后删掉它，让下一轮接入重新抓。
//!
//! 由此推出一条**不显然但很重要**的约束：只有「接入」才可以用这个写入器。任何**别的**
//! 功能顺手用它，就会把「接入前」这个时间点提前到那件事之前 —— 见
//! [`write_without_locking_snapshot`] 里记的那次真实事故。

use crate::error::{AppError, AppResult};
use super::BACKUP_SUFFIX;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(super) fn read_json_or_empty(path: &Path) -> AppResult<Value> {
    if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            Ok(Value::Object(Default::default()))
        } else {
            serde_json::from_str(&raw)
                .map_err(|e| AppError::ToolConfig(format!("解析 {} 失败: {e}", path.display())))
        }
    } else {
        Ok(Value::Object(Default::default()))
    }
}

/// 备份原文件（若存在），然后原子写入新 JSON
pub(super) fn backup_and_write_json(path: &Path, value: &Value) -> AppResult<()> {
    let data = serde_json::to_vec_pretty(value)?;
    backup_and_write_bytes(path, &data)
}

/// 原子写，**不碰 `.bak` / 不落「凭空新建」标记**。给「不是接入」的那些写入用。
///
/// # 这个函数是为一次真实事故建的
///
/// MCP 注册/注销原先也走 [`backup_and_write_bytes`]。它是**首写即锁**的，于是
/// 「接入前快照」的时间点被悄悄提前到了「SynaRoute 第一次因为**任何**理由碰这个文件之前」
/// —— 而开大脑聚合开关（写 MCP）通常发生在点「启动」（接入）**之前**。
///
/// 用户机器上的实证（2026-08-26 读盘）：`~/.codex/config.toml.synaroute.bak` 的时间戳是
/// 8-16，里面带着 `[mcp_servers.synaroute] url = "http://127.0.0.1:9527/mcp"` ——
/// 一个早已废弃的 HTTP 形态、端口也早死了。而那之后（8-22）SynaRoute 自己又改过 config.toml
/// （日志有「已从 Codex 移除 MCP」），`.bak` 却因为已上锁而没有更新。
/// 今天点一次「还原」，就会把这份陈旧快照**整份写回**：Codex 里凭空多出一个连不上的
/// synaroute MCP，用户此后自己加的配置段被抹掉，而界面显示「已从备份还原」= 成功。
///
/// # 为什么 MCP 注册**不需要** `.bak`
///
/// 它的逆操作是精确的 `remove(mcp_servers.synaroute)`（`unregister_mcp_client` 就做这件事），
/// 本来就用不着整文件快照。而 `restore_client_config_keeping_mcp` 会在还原后幂等补回 MCP，
/// 所以这条改动不会让聚合工具丢。
///
/// # 已知的、可接受的代价
///
/// 机器上原本**没有**该配置文件时（未装过 `~/.claude.json` 的新用户），MCP 注册会凭空建出它，
/// 而现在不落「凭空新建」标记 → 注销之后可能留下一份只剩 `{}` 的空文件。
/// 与上面那个「还原写回陈旧全量配置」相比，代价小得多：空文件对客户端无害，
/// 而写回陈旧配置会**抹掉用户的真实改动**。
pub(super) fn write_without_locking_snapshot(path: &Path, data: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::secret::atomic_write(path, data)
}

/// [`write_without_locking_snapshot`] 的 JSON 版。
pub(super) fn write_json_without_locking_snapshot(path: &Path, value: &Value) -> AppResult<()> {
    let data = serde_json::to_vec_pretty(value)?;
    write_without_locking_snapshot(path, &data)
}

pub(super) fn backup_and_write_bytes(path: &Path, data: &[u8]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 幂等：新内容与磁盘现有内容完全一致时，不备份也不写盘（连 .bak 的存在性判定都不必做）。
    if path.exists() {
        if let Ok(current) = std::fs::read(path) {
            if current == data {
                return Ok(());
            }
        }
    }
    // 规则1：改写前备份原文件，但**首写即锁**——`.bak` 已存在就绝不覆盖。
    //
    // 为什么不能只靠「内容相等」守卫（数据丢失级教训）：那条守卫只挡住「重复接入且内容逐字节
    // 相同」这一种。真实场景里重复接入的内容几乎总会变——改代理端口、可服务模型列表变化、
    // 升级换目录导致 MCP 的 command 路径变化——于是守卫不命中，第二次接入就把**已接入态**
    // 拷进 `.bak`，冲掉用户接入前的原始快照。此后点「还原」拿回的是「已接入 v1」，
    // 官方 config / ChatGPT OAuth 登录再也回不来（CLI 与 Codex 的 restore 直接读 `.bak`）。
    //
    // 首写即锁后，`.bak` 恒为「SynaRoute 第一次动这个文件之前」的内容，重复接入多少次都不变质。
    // 配套：`restore_one` 还原成功后删掉 `.bak`（见该函数），让下一轮接入重新抓一份新鲜快照，
    // 否则锁死的旧快照会在「接入→还原→改配置→再接入→再还原」时把用户改动覆盖回很久以前。
    // 「锁」只在两者都还没锁过时才可能上锁，且**互斥**——上了哪一把就不会再上另一把。
    //
    // 这一点必须严格保证：若已经锁了 marker（首次接入时文件不存在），此后 SynaRoute 自己
    // 把文件从无写到有，`path.exists()` 就会变真。若这时只看 `path.exists()` 决定走
    // 「备份」分支，会把 SynaRoute 自己写的内容当成「原始快照」拷进 `.bak`——
    // 于是磁盘上 marker 与 `.bak` 同时存在，`restore_one` 只看 `.bak`（判断顺序在它之前），
    // 就会把文件「还原」成 SynaRoute 自己写的版本，而不是删除它。凭空新建的文件从此永远
    // 「还原」不回「不存在」这个真实的原始状态。
    let backup = backup_path_for(path);
    let marker = created_marker_path_for(path);
    if !backup.exists() && !marker.exists() {
        if path.exists() {
            std::fs::copy(path, &backup)?;
        } else {
            // 文件接入前**根本不存在**（如未装 settings.json 的 Claude CLI 用户、只走
            // ChatGPT 登录从未生成过 config.toml 的 Codex 用户）：没有「原内容」可备份，
            // 落一个空标记代替 `.bak`，供 `restore_one` 判定「这份文件是我凭空造出来的，
            // 该整份删掉」而非「无备份、之前就没接入过」。少这一步的后果：`restore_one`
            // 因 `.bak` 不存在直接判定「无需还原」，文件却原样留在盘上、指着一个已经
            // 无人监听的本地端口——客户端从此永久连不上，卸载之后连能改它的界面都没有了。
            std::fs::write(&marker, b"")?;
        }
    }
    // 规则2：原子写
    crate::secret::atomic_write(path, data)
}

/// 多文件写入的整体回滚（借鉴 cc-switch 的 with_rollback）。
///
/// 场景：一次接入要动多个文件（如 Codex 的 config.toml + auth.json）。若中途某步失败，
/// 已写的文件会留下「半配置」状态。此辅助先对每个目标文件拍快照（内容 or「原本不存在」），
/// 执行闭包；闭包返回 Err 时把所有文件恢复到执行前的状态，避免部分写入。
///
/// 快照失败（无法读原文件）直接返回错误、不执行闭包——宁可不写，也不在无法回滚时冒险。
///
/// # 副文件（`.synaroute.bak` / `.synaroute-created`）**必须一并纳入**
///
/// 这不是洁癖，是一条数据丢失级缺陷的修复。`backup_and_write_bytes` 除了写主文件，还会
/// 「首写即锁」地落下 `.bak` 或 `.synaroute-created` 标记。若只回滚主文件，标记会**留在盘上**，
/// 而它的语义是「这份文件是 SynaRoute 凭空造出来的、还原时该整份删掉」。于是：
///
/// 1. 机器上本无 `~/.codex/auth.json`（只走 ChatGPT 登录的新装机）→ 首次接入落下标记；
/// 2. 读回校验失败（正是「Codex 正在运行并重写 config.toml」那个已知场景）→ 回滚删掉
///    auth.json，**标记留着**；
/// 3. 用户随后 `codex login` 拿回真 OAuth；
/// 4. 下一次接入：`!backup.exists() && !marker.exists()` 因标记在而为假 → 整块备份代码被跳过，
///    真 OAuth 被直接覆盖、`.bak` 从未生成；
/// 5. 点还原 → 走标记支路 `remove_file` → 用户的 ChatGPT 登录态**永久消失，盘上一份副本都没有**。
///
/// 全程静默。故快照集必须是「主文件 + 它的两个副文件」的闭包。
pub(super) fn with_rollback<T>(
    paths: &[PathBuf],
    op: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    // 主文件 + 副文件一起纳入。副文件按主路径推导，调用方无需（也不该）自己列举 ——
    // 让调用方记得列会漏，而漏掉的表现是静默的。
    let mut tracked: Vec<PathBuf> = Vec::with_capacity(paths.len() * 3);
    for p in paths {
        tracked.push(p.clone());
        tracked.push(backup_path_for(p));
        tracked.push(created_marker_path_for(p));
    }

    // 拍快照：Some(bytes)=原内容；None=原本不存在（回滚时应删除）。
    let mut snapshots: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::with_capacity(tracked.len());
    for p in &tracked {
        let snap = if p.exists() {
            Some(std::fs::read(p).map_err(|e| {
                AppError::ToolConfig(format!("回滚快照失败(读 {}): {e}", p.display()))
            })?)
        } else {
            None
        };
        snapshots.push((p.clone(), snap));
    }

    match op() {
        Ok(v) => Ok(v),
        Err(e) => {
            // 尽力回滚：逐个恢复原状。回滚本身的错误不覆盖原始错误，仅告警。
            for (p, snap) in &snapshots {
                let restored = match snap {
                    Some(bytes) => crate::secret::atomic_write(p, bytes),
                    // 原本不存在 → 删除。文件本就不在时 `remove_file` 会报 NotFound，
                    // 那不是错误（回滚的目标状态已达成），不该刷一条误导性告警。
                    None if !p.exists() => Ok(()),
                    None => std::fs::remove_file(p).map_err(AppError::from),
                };
                if let Err(re) = restored {
                    tracing::warn!("接入写入失败后回滚 {} 出错: {re}", p.display());
                }
            }
            Err(e)
        }
    }
}

/// 还原前保留现场用的后缀：`<原名>.synaroute-prerestore`。
///
/// 存在的理由：`.bak` 是**首写即锁**的「接入前快照」，可能已是几个月前的内容，而用户在
/// 那之后往同一文件里写了大量自有配置（Claude Code 每点一次「don't ask again」就往
/// `permissions.allow` 追加一条，还有 hooks / statusLine / 自定义 env；Codex 那边是
/// `[mcp_servers.*]` / `[profiles.*]` / 项目信任项）。还原是**整文件覆盖**，这些改动会
/// 一次性消失，而原先连一份副本都不留 —— 卸载场景尤其致命：能重建它的应用同时也被删了。
/// 留这一份就把「不可逆」变成「可回滚」，与项目「改配置前必先备份」的硬规则一致。
pub(super) const PRERESTORE_SUFFIX: &str = "synaroute-prerestore";

/// 某文件对应的 `.synaroute.bak` 备份路径。
pub(super) fn backup_path_for(path: &Path) -> PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.{BACKUP_SUFFIX}"),
        None => BACKUP_SUFFIX.to_string(),
    })
}

/// 某文件「还原前现场」的保留路径。
pub(super) fn prerestore_path_for(path: &Path) -> PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.{PRERESTORE_SUFFIX}"),
        None => PRERESTORE_SUFFIX.to_string(),
    })
}

/// 「接入前该文件根本不存在」的标记后缀：`<原名>.synaroute-created`。
///
/// 与 `.bak`（有原内容可还原）互斥、二选一——见 [`backup_and_write_bytes`] 的写入侧。
pub(super) const CREATED_MARKER_SUFFIX: &str = "synaroute-created";

/// 某文件对应的「凭空新建」标记路径。
pub(super) fn created_marker_path_for(path: &Path) -> PathBuf {
    path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.{CREATED_MARKER_SUFFIX}"),
        None => CREATED_MARKER_SUFFIX.to_string(),
    })
}

/// 从 `.synaroute.bak` 还原单个文件；无 `.bak` 也无「凭空新建」标记则返回 false（跳过，不报错）。
///
/// **覆盖前先把当前内容另存一份** `.synaroute-prerestore`（见 [`PRERESTORE_SUFFIX`]）：
/// `.bak` 是首写即锁的旧快照，整文件写回会抹掉用户此后所有自有配置，而卸载路径上
/// 这一步之后应用就没了，没有任何界面能帮用户找回。留一份即可回滚。
/// 保留失败**不阻断还原**（还原本身是用户要的），只告警——但会跳过删 `.bak`，
/// 保证任意时刻盘上至少有一份副本。
///
/// 还原成功后**删除 `.bak`**：与 `backup_and_write_bytes` 的「首写即锁」配套。
/// 备份既已交还给目标文件，就该让下一轮接入重新抓一份新鲜的「接入前快照」——否则被锁死的旧
/// 快照会在「接入 → 还原 → 用户改配置 → 再接入 → 再还原」时把用户改动覆盖回很久以前的状态。
/// 副作用（可接受且语义准确）：连点两次还原，第二次会返回「无备份，无需还原」。
/// 删除失败只告警不上抛：还原本身已成功，不该因清理失败把成功报成失败。
///
/// **无 `.bak` 但有「凭空新建」标记**：说明这份文件是 SynaRoute 首次接入时从零建出来的，
/// 接入前的「原状」就是「不存在」。直接删掉整个文件即是精确还原——不是「整文件回滚到某个
/// 旧版本」，而是「回到那个版本原本没有这份文件」的状态。这一支必须存在：没有它，
/// 未装过 `~/.claude/settings.json` 的 Claude CLI 用户、只走 ChatGPT 登录从未生成过
/// `~/.codex/config.toml` 的 Codex 用户，停止/还原/卸载三条路径都会因为「无 .bak」直接
/// 判定「无需还原」而放过这份文件——它会原样留在盘上、永久指向一个已经没人监听的本地端口，
/// 客户端从此连不上，且卸载之后连能改它的界面都没有了。
pub(super) fn restore_one(path: &Path) -> AppResult<bool> {
    let backup = backup_path_for(path);
    if !backup.exists() {
        let marker = created_marker_path_for(path);
        if !marker.exists() {
            return Ok(false);
        }
        // 凭空新建的文件：删除即是还原。删除失败保留标记（下次还能再试），
        // 成功才清标记——与 `.bak` 分支「还原成功才清理凭据」同一条纪律。
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        if let Err(e) = std::fs::remove_file(&marker) {
            tracing::warn!("还原后清理新建标记 {} 失败: {e}", marker.display());
        }
        return Ok(true);
    }
    let data = std::fs::read(&backup)?;

    // 保留现场：目标文件存在才需要留（不存在意味着没什么会被覆盖）。
    let mut kept_scene = true;
    if path.exists() {
        if let Err(e) = std::fs::copy(path, prerestore_path_for(path)) {
            kept_scene = false;
            tracing::warn!(
                "还原前保留现场失败 {}: {e}（将保留 .bak 不删，确保仍有副本可回滚）",
                path.display()
            );
        }
    }

    crate::secret::atomic_write(path, &data)?;

    // 现场没留住就不删 `.bak`：宁可下次还原报「无备份」，也不能出现
    // 「旧快照已删、当前内容已被覆盖」这种两头皆空的状态。
    if kept_scene {
        if let Err(e) = std::fs::remove_file(&backup) {
            tracing::warn!("还原后清理备份 {} 失败: {e}", backup.display());
        }
    }
    Ok(true)
}
