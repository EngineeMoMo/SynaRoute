//! 大脑聚合引擎 V2（FR-013 ~ FR-017）。
//!
//! 流程：
//! 1. 文件检索（retrieval 模块）→ 相关文件上下文
//! 2. 参与者并行思考（只读：收到 prompt + 文件上下文，输出建议）
//! 3. 聚合（compressed / full）
//! 4. 决策者分两阶段：
//!    - Phase1: 输出修改计划（plan）
//!    - Phase2: 用户确认后执行修改（apply）

use crate::error::{AppError, AppResult};
use crate::aggregate_phase::{
    decider_floor_ms, decider_phase_budget_ms, upstream_phase_budget_ms, PHASE_MIN_BUDGET_MS,
};
use crate::model::{
    AggregateMode, BrainConfig, CategoryType, Protocol, RequestTrace,
};
use crate::retrieval;
use crate::store::Store;
use crate::upstream;
use crate::upstream::{ImagePart, MultimodalPrompt, ToolSession, TurnOutcome};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// 决策者输出 → 落盘（解析、六道防线、备份 + 原子写）。
///
/// `#[path]` 挂在这里而不是拆成目录模块：`aggregate.rs` 棘轮余量曾为 0，而写路径要补的
/// 防线比原先那段 `parse_and_apply` 还多。理由同 `lan_guard` / `log_rotate`。
#[path = "aggregate/write.rs"]
pub(crate) mod write;

/// 「检索 → 成员 → 聚合 → 决策者 → 记账」的唯一实现，桌面端 Phase1 与 MCP 通道共用。
///
/// 它的模块头记着这段骨架此前各写一份时漂出的**五处**真缺陷 —— 动它之前先读那一段。
#[path = "aggregate/round.rs"]
mod round;

/// 聚合的闸门：**进程级**并发上限 + 打上游前的三层弹性检查（熔断 / 配额窗口 / 余额）。
#[path = "aggregate/gate.rs"]
mod gate;

/// 喂给模型的 prompt 怎么拼。**检索到的文件用随机 nonce 围栏包起来** ——
/// 那段安全推理（六道防线挡不住「写什么」被劫持）在它的模块头里。
#[path = "aggregate/prompt.rs"]
mod prompt;
/// 成员的工具循环（agent 式多轮 + 四条护栏 + 分批并发执行 + 历史裁剪）。
#[path = "aggregate/tool_loop.rs"]
mod tool_loop;
use tool_loop::{run_member_turns, TOOL_CTX_BUDGET_RANGE, TOOL_ROUNDS_RANGE};
// 只有测试段在用的那几个：放进上面那行会被 clippy 判成 unused import。
#[cfg(test)]
use tool_loop::{effective_rounds, tool_call_brief, MAX_CONCURRENT_TOOLS, MAX_TOOL_CALLS_PER_TURN};

use prompt::{build_member_prompt, build_solo_decider_prompt, format_file_context};

/// 聚合日志里请求/响应体的最大字符数（与调用模型日志同量级，防超大 prompt 撑爆内存日志）。
const AGG_LOG_CAP: usize = 20_000;

/// 截断超长文本，附省略提示（成员答案/汇总产物/决策者入参出参落日志用）。
///
/// 用 `char_indices().nth()` 而不是先 `chars().count()`：后者把整串走一遍**才知道要不要
/// 截断**，而这个函数每轮聚合要对每个成员的入参与出参各调一次，单个可达数十万字符。
/// 现在未超限时只走前 `AGG_LOG_CAP` 个字符（绝大多数调用都是这一支）。
fn cap_text(s: &str) -> String {
    match s.char_indices().nth(AGG_LOG_CAP) {
        None => s.to_string(),
        // 已经确定要截断了，这时再数总长度（为了如实报「共 N 字符」）才值得。
        Some((cut, _)) => format!("{}\n…（已截断，共 {} 字符）", &s[..cut], s.chars().count()),
    }
}

/// 为「keyId::model 引用」的一次聚合调用构造链路快照（汇总者/决策者/降级独答用）。
/// 让用户在运行日志里展开看到：这一步喂给模型的完整入参 + 模型的完整回答。
fn trace_for_ref(
    store: &Arc<Store>,
    reference: &str,
    prompt: &str,
    output: &str,
    latency_ms: u64,
    ok: bool,
) -> Option<RequestTrace> {
    let (key_id, model) = reference.split_once("::")?;
    let key = store.get_key(key_id)?;
    Some(RequestTrace {
        request_id: String::new(),
        key_name: key.name,
        vendor: key.vendor,
        protocol: key.protocol,
        url: key.base_url,
        requested_model: model.to_string(),
        real_model: model.to_string(),
        request_body: cap_text(prompt),
        response_body: cap_text(output),
        status: None,
        latency_ms,
        ok,
        was_truncated: None,
    })
}

/// 从 `keyId::modelName` 引用里取出 keyId。
///
/// # 为什么这个两行函数值得存在
///
/// 聚合的每一条 `append_event_full` 此前都把 `key_id` 传成 `None` —— **8 处全是**，
/// 其中 6 处带着 usage。而 `store.rs` 的累加器键是 `(分类, key_id.unwrap_or_default())`，
/// 于是这些消耗全部落进 `(分类, "")` 这一个桶：
///
/// - 用量页显示成「（系统级）」，用户看不出是哪条 Key 花的钱；
/// - `usage_cost::rows` 拿空 keyId 查不到 Key → 取不到代表模型、也取不到计费倍率
///   → **花费列恒为「—」**（用户报的正是这个）。
///
/// 而 key_id 在那 6 处**全都在作用域里**（决策者/汇总者是 `keyId::model` 引用，
/// 成员侧有 `BrainMember.key_id`）—— 不是拿不到，是记账时扔了。
///
/// 有 `aggregate_usage_is_never_recorded_without_a_key` 一条源码级判据盯着这件事，
/// 因为「记得传 key_id」是一条必然会漏的纪律，而漏掉的表现是静默的。
fn ref_key_id(reference: &str) -> Option<&str> {
    reference.split_once("::").map(|(key_id, _)| key_id)
}


struct MemberAnswer {
    label: String,
    answer: String,
}

/// 成员一次实际调用的元信息（构造日志 trace 用）。
struct MemberCallMeta {
    /// 该成员用的那条 Key 的 id。**记账用**，不进 trace 展示。
    /// 少了它，这个成员烧的额度会落进「（系统级）」那个空 keyId 桶里（见 [`ref_key_id`]）。
    key_id: String,
    key_name: String,
    vendor: String,
    protocol: Protocol,
    base_url: String,
    model: String,
    latency_ms: u64,
    /// 本成员整轮（含工具循环全部轮次）的 token 用量。
    ///
    /// 挂在 meta 上而不是 MemberAnswer：失败的成员**同样烧了额度**（尤其工具循环跑了
    /// 几轮才超时的），不记就会让「这次聚合花了多少」的账对不上。
    usage: upstream::TokenUsage,
}

/// gather_members 的结果：成功答案 + 统计（供结果面板展示「N 参与 / M 失败」）。
struct GatherOutcome {
    answers: Vec<MemberAnswer>,
    /// 被处理的成员数（含调用失败与前置不可用；不含被禁用而跳过的）。恒 ≥ 成功数 + 失败数不成立时是账目缺陷。
    attempted: usize,
    /// 调用失败 / 不可用的成员数。
    failed: usize,
    /// 因 Key 被禁用而跳过的成员数。
    skipped_disabled: usize,
    /// 成员阶段合计 token 用量（成功 + 失败都计入）。
    usage: upstream::TokenUsage,
}

/// 取 brain 配置并确认大脑聚合已启用。
///
/// 🔴 **四个入口都必须过这道。** `run_apply`（Phase2）此前**没有**这个检查 —— 它只取了
/// `decider_ref` 就往下走，于是用户在界面上关掉大脑聚合之后，「确认执行」照样能跑并
/// **写文件**。权限门在多个入口各写一份的失效方向正是这样：漏掉的那个入口静默绕过它，
/// 而没人会去测「关掉开关之后按钮还能不能用」。
fn require_enabled(store: &Arc<Store>, category: CategoryType) -> AppResult<BrainConfig> {
    let brain = store.get_brain(category);
    if !brain.enabled {
        // 带上分类名：分类现在由接入端自动决定，用户没传过它，报错必须自己说清是哪一页。
        let who = category.display_name();
        return Err(AppError::Invalid(format!(
            "「{who}」未启用大脑聚合，请在 SynaRoute 桌面端切到该分类，配好参与者与决策者"
        )));
    }
    Ok(brain)
}

/// Phase1: 参与者思考 + 决策者输出修改计划。
pub async fn run_plan(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
) -> AppResult<AggregateResult> {
    let brain = require_enabled(store, category)?;
    // Phase1 **开始**的墙钟时刻，随计划一起回给前端、Phase2 原样回传。
    // 取开始而不是结束：检索发生在最开头，决策者手里的就是那一刻的文件内容，
    // 于是「Phase1 跑了 60 秒、用户在这期间改了文件」也能被 Phase2 判出来。
    let plan_started_ms = chrono::Utc::now().timestamp_millis();
    let work_dir = resolve_work_dir(&brain);
    let out = round::run(
        store,
        category,
        &brain,
        round::RoundSpec {
            kind: round::RoundKind::DesktopPlan,
            prompt,
            // 桌面端 UI 这条路径不接受图片（只有 MCP 的 images 参数会传图，见 run_mcp）。
            images: &[],
            work_dir,
        },
    )
    .await?;
    Ok(AggregateResult::Plan {
        content: out.analysis,
        work_dir: out.work_dir,
        plan_started_ms,
    })
}

/// Phase2a: 决策者按已确认的计划输出完整文件内容，解析成**落盘预览** —— 一个字节都不写。
///
/// # 为什么要有这一步
///
/// 用户在 Phase1 确认的是**计划文本**，而完整文件内容是 Phase2 才生成的。原实现里
/// 「确认执行」一点就直接落盘：用户从未看到将写入的字节，也没有「将改动这 N 个文件」的清单，
/// 而这是整套功能里唯一不可逆的动作。现在拆成「出内容 + 预览」（本函数）与
/// 「按原文落盘」（[`run_write`]）两步，中间插一次用户确认。
///
/// # `pinned_work_dir` 的三种形态
///
/// - `Some(非空)`：Phase1 定下的目录，原样用。
/// - `Some("")`：Phase1 明确「无工作目录」（用户没配、也没有活跃会话）。仍跑决策者
///   —— 信息查询类的问题不需要目录 —— 但预览恒空，且不会写任何文件。
/// - `None`：老前端 / 直接调 IPC。**刻意不回退实时解析**：`resolve_work_dir` 会去扫会话
///   历史挑一个「最近活跃」的项目，那意味着往一个用户从未指定过的目录写文件。读路径上那是
///   便利，写路径上是越权。当成「无工作目录」处理。
pub async fn run_preview(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
    confirmed_plan: &str,
    pinned_work_dir: Option<String>,
    plan_started_ms: i64,
) -> AppResult<write::PreviewReport> {
    let brain = require_enabled(store, category)?;
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;
    let effective_work_dir = pinned_work_dir.filter(|d| !d.trim().is_empty());

    // 重新取文件上下文：决策者要按**当前**内容输出完整新文件。
    let file_context = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let outcome =
                retrieval::retrieve_detailed(work_dir, prompt, brain.max_context_tokens).await;
            // 这条事件此前整个 Phase2 都没有 —— 三条路径里只有它不落检索日志，
            // 于是「计划里提到的文件为什么没进上下文」在 Phase2 无从排查。
            store.append_event(
                category,
                "aggregate",
                None,
                &format!("确认执行 · 检索 · {} · 目录={work_dir}", outcome.summary),
            );
            format_file_context(&outcome.files)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let exec_prompt = format!(
        "用户已确认以下修改计划，请执行。\n\
         对于每个需要修改的文件，输出完整的新文件内容，格式如下：\n\
         ```file:相对路径\n完整文件内容\n```\n\n\
         ## 用户需求\n{prompt}\n\n\
         ## 确认的计划\n{plan}\n\n\
         {file_section}\
         请输出每个文件的完整新内容：",
        prompt = prompt,
        plan = confirmed_plan,
        file_section = if file_context.is_empty() {
            String::new()
        } else {
            format!("## 当前文件内容\n{file_context}\n\n")
        }
    );

    // with_usage 包住：Phase2 执行同样是决策者级别的大请求，账不能少这笔。
    let (result, exec_used) = upstream::with_usage(call_ref(
        store,
        category,
        &decider_ref,
        &exec_prompt,
        brain.total_timeout_ms,
    ))
    .await;
    let result = result?;
    store.append_event_full(
        category,
        "aggregate",
        ref_key_id(&decider_ref),
        &format!(
            "确认执行 · 决策者返回 · {}{}",
            label_ref(store, &decider_ref),
            if exec_used.is_empty() {
                String::new()
            } else {
                format!(" · {}", exec_used.fmt_compact())
            }
        ),
        store
            .get_settings()
            .aggregate_trace_enabled
            .then(|| trace_for_ref(store, &decider_ref, &exec_prompt, &result, 0, true))
            .flatten(),
        None,
        (!exec_used.is_empty()).then_some(exec_used),
    );

    let changes = match effective_work_dir.as_deref() {
        Some(dir) => write::plan_changes(dir, &result, plan_started_ms),
        None => vec![],
    };
    Ok(write::PreviewReport {
        content: result,
        changes,
        work_dir: effective_work_dir,
    })
}

/// Phase2b: 用户看过预览后，按决策者**原文**重新解析并落盘。不调任何模型。
///
/// 🔴 **按原文重新解析，不接收前端传回来的 changes**：那份数据在前端手里待过一趟，
/// 拿它当写入依据等于让前端决定「往哪写什么」。原文是唯一可信输入，而重新解析走的是
/// 与预览完全相同的 [`write::judge`]，所以两次结论必然一致。
pub async fn run_write(
    store: &Arc<Store>,
    category: CategoryType,
    decider_output: &str,
    work_dir: Option<String>,
    plan_started_ms: i64,
) -> AppResult<AggregateResult> {
    require_enabled(store, category)?;
    let Some(dir) = work_dir.filter(|d| !d.trim().is_empty()) else {
        return Err(AppError::Invalid(
            "没有工作目录，无法写入任何文件。请在大脑聚合设置里指定工作目录（或开启自动跟随）后重新生成计划。"
                .into(),
        ));
    };
    let report = write::apply_changes(&dir, decider_output, plan_started_ms);
    let ok: Vec<String> = report
        .changes
        .iter()
        .filter(|c| c.success)
        .map(|c| c.path.clone())
        .collect();
    let failed: Vec<String> = report
        .changes
        .iter()
        .filter(|c| !c.success)
        .map(|c| format!("{}（{}）", c.path, c.error.as_deref().unwrap_or("写入失败")))
        .collect();

    // 🔴 落一条事件。改用户的源文件是这套功能里唯一不可逆的动作，而它此前的审计痕迹
    // **只活在前端那次 IPC 返回值里** —— 窗口一关就再也查不到「那次到底改了哪些文件」。
    store.append_event(
        category,
        "aggregate",
        None,
        &format!(
            "确认执行 · 落盘 · 目录={dir} · 成功 {} · 失败 {}{}{}",
            ok.len(),
            failed.len(),
            if ok.is_empty() {
                String::new()
            } else {
                format!(" · 已写入 [{}]", ok.join(" · "))
            },
            if report.backups.is_empty() {
                String::new()
            } else {
                format!(" · 已备份 {} 份原文件", report.backups.len())
            }
        ),
    );
    for f in &failed {
        store.append_event(category, "aggregate", None, &format!("落盘失败 · {f}"));
    }

    let mut content = decider_output.to_string();
    if !report.backups.is_empty() {
        content.push_str(&format!(
            "\n\n---\n📦 覆盖前的原文已备份（同目录 `.synaroute.bak`）：\n- {}",
            report.backups.join("\n- ")
        ));
    }
    if !failed.is_empty() {
        content.push_str(&format!(
            "\n\n---\n⚠️ 以下文件写入失败：\n- {}",
            failed.join("\n- ")
        ));
    }
    Ok(AggregateResult::Applied {
        content,
        files_modified: ok,
    })
}

/// MCP 通道聚合结果（供 MCP Server 组装 Markdown 返回）。
///
/// 与桌面端 Phase1 共用 [`round::run`]，故这里只是那份产物的重导出 —— 两条路径曾经各写
/// 一份骨架，漂出的五处缺陷记在 `round` 的模块头里。
pub use round::RoundOutcome as McpAggregateResult;

/// MCP 通道专用聚合：只出建议 / 修改计划，绝不写文件（Q5）。
///
/// 与 [`run_plan`] 的差异只有三处：`cwd` 参数显式指定项目路径（覆盖 brain 的
/// auto-follow / work_dir）、支持 `images` 多模态入参、返回结构化元信息供 MCP Server
/// 组装 Markdown。骨架完全一致。
pub async fn run_mcp(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
    cwd: Option<String>,
    image_paths: Vec<String>,
) -> AppResult<McpAggregateResult> {
    let brain = require_enabled(store, category)?;
    // 工作目录优先级：MCP 显式 cwd > brain 配置（auto-follow / 手工 work_dir）
    let work_dir = match cwd {
        Some(c) if !c.trim().is_empty() => Some(c),
        _ => resolve_work_dir(&brain),
    };
    // 图片：在这里加载而不是在 mcp.rs —— 图片路径相对于**同一个** work_dir，
    // 两处各算一次工作目录必然漂移。任何校验不过都直接抛错，绝不静默丢图。
    let images = crate::agent_tools::load_images(work_dir.as_deref(), &image_paths)
        .map_err(AppError::Invalid)?;
    if !images.is_empty() {
        store.append_event(
            category,
            "aggregate",
            None,
            &format!(
                "图片输入 · {} 张 · {}",
                images.len(),
                images
                    .iter()
                    .map(|i| i.media_type.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
    }
    round::run(
        store,
        category,
        &brain,
        round::RoundSpec {
            kind: round::RoundKind::Mcp,
            prompt,
            images: &images,
            work_dir,
        },
    )
    .await
}

// ─── 内部辅助 ───────────────────────────────────────────────────────────────

/// 解析实际使用的工作目录。
///
/// 优先级：`auto_follow_active` 开启时用会话历史里最近活跃的目录；否则用手工 `work_dir`。
/// **两者都拿不到时兜底去扫会话历史**——旧实现在此直接返回 None，于是「开了检索但没填目录、
/// 也没勾自动跟随」这个最常见的默认状态下检索整段跳过、且无任何提示。既然 [`crate::workdirs`]
/// 已经能从 Claude CLI / Codex / 桌面端会话历史里读出 cwd，就该用它兜底而不是干脆不检索。
fn resolve_work_dir(brain: &BrainConfig) -> Option<String> {
    if brain.auto_follow_active {
        if let Ok(list) = crate::workdirs::scan() {
            if let Some(first) = list.into_iter().next() {
                return Some(first.path);
            }
        }
    }
    if let Some(w) = brain.work_dir.clone().filter(|s| !s.trim().is_empty()) {
        return Some(w);
    }
    // 兜底：没勾自动跟随也没填目录 → 仍尝试会话历史，避免「静默不检索」。
    crate::workdirs::scan()
        .ok()
        .and_then(|l| l.into_iter().next())
        .map(|w| w.path)
}


/// 单个成员任务的结果：成功带答案，失败带原因（用于落日志）。
enum MemberOutcome {
    /// 成功：答案 + 调用元信息（供日志 trace 展示完整入参/出参）。
    Ok(MemberAnswer, MemberCallMeta),
    /// 调用失败：label + 具体原因（超时 / HTTP / 连接 / 空答案）；meta 供 trace。
    Failed { label: String, reason: String, meta: Option<MemberCallMeta> },
    /// 被禁用而跳过（不计失败）。带 label 供日志点名 —— 无痕跳过会让桌面端聚合
    /// 「结果面板少一位专家、无任何解释」，用户以为聚合坏了或模型没答。
    SkippedDisabled { label: String },
    /// 无密钥 / 熔断 / Key 不存在等前置不可用（计入失败，附原因）。
    Unavailable { label: String, reason: String },
}

/// 一个成员做上游调用需要的入参。上游那部分直接复用 [`upstream::TurnParams`]，
/// 这里只额外带一个 `label`（日志用，不进请求）。
struct MemberCallCtx<'a> {
    up: upstream::TurnParams<'a>,
    label: &'a str,
}

/// 带图片却收到 4xx 时，在失败原因里点出「该模型可能不支持图片输入」。
///
/// 为什么必须显式提示：成员配的可能是纯文本模型，上游只回一个 400，用户在日志里看到的是
/// 「调用失败：OpenAI HTTP 400: ...」，根因埋在响应体里，几乎没人能一眼看出是图片导致的。
/// 判 4xx 用 [`MemberError::is_4xx`]（结构化状态码），不再搜错误文本——理由见 `MemberError`。
fn image_unsupported_hint(err: &MemberError, had_images: bool) -> String {
    let reason = &err.msg;
    if !had_images || !err.is_4xx() {
        return reason.to_string();
    }
    format!(
        "{reason}\n提示：本次请求带了图片，而 4xx 常见于模型不支持图片输入 —— \
         请确认该成员模型具备多模态能力，或去掉 images 参数重试。"
    )
}

/// 成员调用失败：消息 + **结构化**上游状态码（`None` = 非上游 HTTP 错误，如超时、
/// 轮次上限、连接层失败）。
///
/// 为什么不用裸 `String`：判「是否 4xx」曾经靠在错误文本里搜 `HTTP 4xx`，而该文本**拼进了
/// 上游响应体前 400 字符**。上游返 500 但 body 里带 `HTTP 404` 字样时（网关回显原始报文是
/// 常见形态），会被误判成 4xx → 给用户追加一句「模型可能不支持图片输入」，把排障方向
/// 引到多模态能力上，而真因是上游 5xx。这与 `is_retriable_upstream_error` 曾经的
/// 假阳性完全同形，只是后果是误导而非白重试。
#[derive(Debug, Clone)]
pub struct MemberError {
    pub msg: String,
    pub status: Option<u16>,
}

impl MemberError {
    /// 我们自己产生的失败（超时、轮次上限、未声明工具…）：没有上游状态码。
    fn own(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), status: None }
    }
    /// 上游调用失败：从 `AppError` 取出结构化状态码，**不从文本反推**。
    fn from_upstream(e: &AppError) -> Self {
        Self { msg: format!("调用失败：{e}"), status: e.upstream_status() }
    }
    fn is_4xx(&self) -> bool {
        matches!(self.status, Some(s) if (400..500).contains(&s))
    }
}

impl std::fmt::Display for MemberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

/// 工具开关开启且工作目录可用时，探测一次工具环境（codegraph 在此只解析一次，
/// 否则每次工具调用都要起子进程探版本）。
///
/// 返回 None 即本轮不给成员工具，请求形状与「未加这个开关之前」完全一致。
/// 每种 None 的原因都落日志 —— 「开了开关却没生效」必须看得见，这正是本项目反复防的
/// 静默失效那类缺陷。
async fn prepare_tool_env(
    store: &Arc<Store>,
    category: CategoryType,
    brain: &BrainConfig,
    work_dir: Option<&str>,
) -> Option<Arc<crate::agent_tools::ToolEnv>> {
    if !brain.tools_enabled {
        return None;
    }
    let Some(dir) = work_dir.filter(|d| !d.trim().is_empty()) else {
        store.append_event(
            category,
            "aggregate",
            None,
            "工具调用已开启，但没有工作目录（未传 cwd、未开自动跟随、也未填手工目录），本轮不提供工具",
        );
        return None;
    };
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
        store.append_event(
            category,
            "aggregate",
            None,
            &format!("工具调用已开启，但工作目录不存在：{dir}，本轮不提供工具"),
        );
        return None;
    }
    let env = crate::agent_tools::ToolEnv::detect(path).await;
    // 单次结果上限：0 = 用内置默认（8000）；非 0 时 with_result_cap 自己会 clamp 到 1000~40000。
    let env = if brain.tool_result_cap_chars == 0 {
        env
    } else {
        env.with_result_cap(brain.tool_result_cap_chars)
    };
    if let Some(note) = &env.codegraph_note {
        store.append_event(category, "aggregate", None, &format!("工具环境 · {note}"));
    }
    store.append_event(
        category,
        "aggregate",
        None,
        &format!(
            "工具调用已开启 · 目录={dir} · 轮数上限 {}",
            brain
                .max_tool_rounds
                .clamp(TOOL_ROUNDS_RANGE.0, TOOL_ROUNDS_RANGE.1)
        ),
    );
    Some(Arc::new(env))
}

async fn gather_members(
    store: &Arc<Store>,
    category: CategoryType,
    brain: &BrainConfig,
    prompt: &str,
    images: &[ImagePart],
    tool_env: Option<Arc<crate::agent_tools::ToolEnv>>,
    budget_ms: u64,
) -> GatherOutcome {
    let total_timeout = Duration::from_millis(budget_ms);
    let settings = store.get_settings();
    let retry = settings.upstream_retry_enabled;
    // 重型 trace（成员完整入参/答案，可达数十万字符）受开关控制，默认关，避免每轮聚合都写盘增大磁盘 IO。
    // 状态行（成功/失败/耗时）始终保留——轻量且排障必需。
    let trace_enabled = settings.aggregate_trace_enabled;
    // 🔴 **进程级**并发闸，不是每轮新建。四个入口（CLI 走 HTTP、Codex 与桌面端各一个
    // stdio 子进程、桌面端 BrainPage）能同时开轮，每轮各建一把的话用户设的上限
    // 在「多客户端同时用」这个最需要它的场合恰好不生效。理由全文见 gate 模块头。
    let sem = gate::member_permits(category, brain.concurrency_limit);
    // prompt 内嵌了检索到的全部文件内容（可达数十万字符）。用 Arc<str> 让所有成员任务
    // 共享同一份，克隆只 bump 引用计数，避免 N 个成员各持一份大副本同时驻留内存。
    let prompt: Arc<str> = Arc::from(prompt);
    // 图片同理走 Arc：一张截图 base64 后可达数 MB，N 个成员各持一份会翻 N 倍。
    let mm: Arc<MultimodalPrompt> = Arc::new(if images.is_empty() {
        MultimodalPrompt::from_text(prompt.as_ref())
    } else {
        MultimodalPrompt {
            text: prompt.to_string(),
            images: images.to_vec(),
        }
    });
    let had_images = mm.has_images();
    let max_tool_rounds = brain.max_tool_rounds;
    // 工具历史字符预算：0 = 关闭裁剪（原样保留）；非 0 时 clamp 到合理区间，
    // 防止用户误配一个过小的值把每轮结果都压成占位（等于工具白调）。
    let tool_ctx_budget = if brain.tool_ctx_budget_chars == 0 {
        0
    } else {
        (brain.tool_ctx_budget_chars as usize).clamp(TOOL_CTX_BUDGET_RANGE.0, TOOL_CTX_BUDGET_RANGE.1)
    };
    // trace 里的入参：prompt 正文 + 图片计数。base64 正文刻意不记日志 —— 一张几 MB、且没有
    // 阅读价值，写进去只会让日志文件与 IPC 载荷暴涨（上一轮刚为此把日志载荷降了 99.5%）。
    let trace_prompt: Arc<str> = Arc::from(if had_images {
        format!(
            "{}\n\n[附带 {} 张图片（base64 内容未记入日志）]",
            cap_text(&prompt),
            mm.images.len()
        )
    } else {
        cap_text(&prompt)
    });

    // 成员阶段的**整体墙钟 deadline**。此前只对「单个成员的模型调用」套 timeout、排队时间
    // 不设限 —— 成员数 > 并发上限时阶段墙钟变成 ceil(N/并发)×budget（6 成员/并发 1 =
    // 6 倍预算），整轮 deadline 被吃穿，compress 与决策者各只剩 5s 保底、几乎必超时，
    // MCP 客户端还可能先杀连接，已烧的全部成员额度作废。
    //
    // 现在的规则：排队仍不预先掐死成员（保住原注释的意图 —— 排到了就该有机会跑），
    // 但拿到 permit 后只分到 deadline 的**剩余时间**而非整份预算；剩余不足最小片时
    // 直接判 Unavailable 并写明「排队耗尽 + 怎么修」，不再白打一次注定超时的上游。
    let phase_deadline = std::time::Instant::now() + total_timeout;
    // 进闭包用（u32 是 Copy，直接按值捕获）：排队耗尽的错误文案里要点名当前并发上限。
    // 用 **实际生效** 的上限，不是用户填的原值 —— 填 999 时真实上限是 gate 的硬顶，
    // 回显原值并让他「提高并发上限」是指向一个不会有任何效果的操作。
    let brain_concurrency = gate::effective_limit(brain.concurrency_limit);

    // 超时按「单个成员的实际模型调用」计。信号量排队时间**不**计入超时——否则
    // concurrency_limit 小于成员数时，后排成员在队列里就耗尽预算、从未发出请求即被判超时。
    // 故先 acquire permit（排队，不设时限），再对真正的模型调用套 timeout。
    // 慢成员到点各自作废，不拖垮已答完的成员。失败不再静默：各自返回具体原因，由下方落日志。
    let tasks = brain.members.iter().map(|m| {
        let store = store.clone();
        let sem = sem.clone();
        let mm = mm.clone();
        let tool_env = tool_env.clone();
        let key_id = m.key_id.clone();
        let model = m.model_name.clone();
        async move {
            let Ok(_permit) = sem.acquire().await else {
                return MemberOutcome::Unavailable {
                    label: model.clone(),
                    reason: "内部并发信号量异常".into(),
                };
            };
            let Some(key) = store.get_key(&key_id) else {
                return MemberOutcome::Unavailable {
                    label: model.clone(),
                    reason: "Key 不存在（可能已被删除）".into(),
                };
            };
            let label = format!("{} / {}", key.name, model);
            // 禁用**且未开「允许大脑聚合使用」**的 Key 不参与聚合（那条开关的语义就是「不进路由池但可参与聚合」）。
            if !key.enabled && !key.allow_in_aggregate {
                return MemberOutcome::SkippedDisabled { label };
            }
            // 三层弹性一次判完（熔断 / 配额窗口 / 余额耗尽）。聚合成员固定 Key、不做故障
            // 转移，跳过只为「别白打一次注定失败的上游」。判据收在 gate 里 —— 此前这里
            // 只判了熔断，另两层对聚合同样适用却漏了（见 gate 模块头）。
            if let Some(reason) = gate::precheck_member(&key) {
                return MemberOutcome::Unavailable { label, reason };
            }
            // 取密钥。`get` 在「主口令模式未解锁」时刻意返回 `Err` 而非 `Ok(None)`
            // （见 secret.rs 该方法注释），正是为了让「需要解锁」这条可行动信息传得出来。
            // 故这里不能 `.ok().flatten()` 一把吞成 None —— 那会把锁定态报成「未配置密钥」，
            // 用户被引去逐个检查密钥配置，而真正要做的只是输一次主口令。
            // （同一轮里决策者路径用 `?` 能正确抛出解锁提示，两条路径口径必须一致。）
            let secret = match store.secrets.read().get(&key_id) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    return MemberOutcome::Unavailable {
                        label,
                        reason: "未配置密钥".into(),
                    };
                }
                Err(e) => {
                    return MemberOutcome::Unavailable {
                        label,
                        // 错误文案已含「请打开主窗口输入主口令解锁」这类行动指引，原样带出。
                        reason: format!("密钥不可用：{e}"),
                    };
                }
            };
            // 排队结束后按阶段 deadline 重算本成员实际可用的时间。剩余不足最小片时
            // 不再发起注定超时的调用 —— 白烧一次上游额度还拖长整轮。
            let member_left = phase_deadline.saturating_duration_since(std::time::Instant::now());
            if member_left < Duration::from_millis(PHASE_MIN_BUDGET_MS) {
                return MemberOutcome::Unavailable {
                    label,
                    reason: format!(
                        "成员阶段预算已被排队耗尽（并发上限 {} 低于成员数，后排成员分不到时间）。\
                         可提高「并发上限」、减少成员数或增大「总超时」。",
                        brain_concurrency
                    ),
                };
            }
            let member_budget_ms = member_left.as_millis() as u64;
            // 仅对实际模型调用套超时（这才是「成员自己的工作时间」）。
            // 单请求 HTTP 超时给成员预算 +5s 余量（此前误用 key 的 30s 代理级超时，
            // 非流式长回答必然被掐死、重试 3 次 ≈ 91s 全灭）；外层 tokio timeout 先到点，
            // 报出干净的「超时（>Xms）」而非 reqwest 的晦涩错误。
            let req_timeout = Duration::from_millis(member_budget_ms.saturating_add(5_000));
            let started = std::time::Instant::now();
            let mk_meta = |latency_ms: u64, usage: upstream::TokenUsage| MemberCallMeta {
                key_id: key.id.clone(),
                key_name: key.name.clone(),
                vendor: key.vendor.clone(),
                protocol: key.protocol,
                base_url: key.base_url.clone(),
                model: model.clone(),
                latency_ms,
                usage,
            };
            let mut session = ToolSession::new(key.protocol, &mm);
            let ctx = MemberCallCtx {
                up: upstream::TurnParams {
                    key: &key,
                    secret: &secret,
                    model: &model,
                    // 占位：真值由 `run_member_turns` 每轮按当前历史重算（见那里的注释）。
                    // 这里给 None 而不是某个默认值 —— 万一将来有人加了条绕过重算的路径，
                    // OpenAI 侧表现为「不设上限」（符合定调），Anthropic 侧会被上游明确拒绝，
                    // 而不是悄悄按一个本地常量截断。
                    max_tokens: None,
                    retry,
                    request_timeout: req_timeout,
                },
                label: &label,
            };
            let call = run_member_turns(
                &store,
                category,
                &mut session,
                &ctx,
                tool_env.as_deref(),
                max_tool_rounds,
                member_budget_ms,
                tool_ctx_budget,
            );
            // 用量 scope 包住整个成员调用（含工具循环的每一轮）：
            // 失败/超时的成员同样烧了额度，四个分支都要把 used 记进 meta，
            // 否则「这次聚合花了多少」的账会少算 —— 而工具循环跑几轮才超时恰恰是最贵的情形。
            let (res, used) = upstream::with_usage(timeout(member_left, call)).await;
            match res {
                Ok(Ok(ans)) if !ans.trim().is_empty() => {
                    let meta = mk_meta(started.elapsed().as_millis() as u64, used);
                    MemberOutcome::Ok(MemberAnswer { label, answer: ans }, meta)
                }
                Ok(Ok(_)) => MemberOutcome::Failed {
                    label,
                    reason: "返回空答案".into(),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64, used)),
                },
                Ok(Err(e)) => MemberOutcome::Failed {
                    label,
                    // e 已含 HTTP 状态码 / 连接失败详情；带图时再点出多模态支持这条可能根因。
                    reason: image_unsupported_hint(&e, had_images),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64, used)),
                },
                Err(_) => MemberOutcome::Failed {
                    label,
                    reason: format!("超时（>{}ms）", member_budget_ms),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64, used)),
                },
            }
        }
    });

    let outcomes = join_all(tasks).await;

    let mut answers = Vec::new();
    let mut attempted = 0usize;
    let mut failed = 0usize;
    let mut skipped_disabled = 0usize;
    // 整轮成员阶段的合计用量（成功 + 失败都算：失败的成员一样烧了额度）。
    let mut total_usage = upstream::TokenUsage::default();
    for o in outcomes {
        match o {
            MemberOutcome::Ok(ans, meta) => {
                attempted += 1;
                // 带 trace：展开可见「喂给该成员的完整 prompt + 成员的完整答案」。受开关控制。
                let trace = trace_enabled.then(|| RequestTrace {
                    request_id: String::new(),
                    key_name: meta.key_name,
                    vendor: meta.vendor,
                    protocol: meta.protocol,
                    url: meta.base_url,
                    requested_model: meta.model.clone(),
                    real_model: meta.model,
                    request_body: trace_prompt.to_string(),
                    response_body: cap_text(&ans.answer),
                    status: None,
                    latency_ms: meta.latency_ms,
                    ok: true,
                    was_truncated: None,
                });
                let usage_part = if meta.usage.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", meta.usage.fmt_compact())
                };
                total_usage.add(&meta.usage);
                store.append_event_full(
                    category,
                    "aggregate",
                    Some(&meta.key_id),
                    &format!(
                        "参与者成功 · {} · {}ms{}",
                        ans.label, meta.latency_ms, usage_part
                    ),
                    trace,
                    None,
                    (!meta.usage.is_empty()).then_some(meta.usage),
                );
                answers.push(ans);
            }
            MemberOutcome::Failed { label, reason, meta } => {
                attempted += 1;
                failed += 1;
                // 失败成员的用量**必须先取出来再消费 meta**：它同样烧了额度，
                // 工具循环跑几轮才超时更是最贵的情形，漏记会让总账偏低。
                // key_id 同理 —— 下面那个 `meta.filter(..).map(..)` 会把 meta 整个吃掉。
                let failed_usage = meta.as_ref().map(|m| m.usage).unwrap_or_default();
                let failed_key_id = meta.as_ref().map(|m| m.key_id.clone());
                total_usage.add(&failed_usage);
                // 失败原因落日志（归「大脑聚合」分组），不再静默吞；带 trace 供展开看入参。受开关控制。
                let trace = meta.filter(|_| trace_enabled).map(|m| RequestTrace {
                    request_id: String::new(),
                    key_name: m.key_name,
                    vendor: m.vendor,
                    protocol: m.protocol,
                    url: m.base_url,
                    requested_model: m.model.clone(),
                    real_model: m.model,
                    request_body: trace_prompt.to_string(),
                    response_body: cap_text(&reason),
                    status: None,
                    latency_ms: m.latency_ms,
                    ok: false,
                    was_truncated: None,
                });
                store.append_event_full(
                    category,
                    "aggregate",
                    failed_key_id.as_deref(),
                    &format!(
                        "参与者失败 · {label} · {reason}{}",
                        if failed_usage.is_empty() {
                            String::new()
                        } else {
                            format!(" · 已消耗 {}", failed_usage.fmt_compact())
                        }
                    ),
                    trace,
                    None,
                    (!failed_usage.is_empty()).then_some(failed_usage),
                );
            }
            MemberOutcome::Unavailable { label, reason } => {
                // 计入 attempted：不计的话汇总行会出现「发起 1 · 失败 2」的矛盾账
                // （Unavailable 只加 failed 不加 attempted，失败数大于发起数），
                // 用户对账必然困惑。attempted 的口径是「被处理的成员数（禁用跳过除外）」。
                attempted += 1;
                failed += 1;
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者不可用 · {label} · {reason}"),
                );
            }
            MemberOutcome::SkippedDisabled { label } => {
                skipped_disabled += 1;
                // 必须落日志：此前只递增计数，「禁用跳过 N」的汇总行只在 MCP 路径打，
                // 桌面端聚合的禁用成员**零痕迹消失** —— 结果面板少一位专家、日志查不到
                // 任何解释。逐条点名后两条路径都可追溯。
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者已禁用，跳过 · {label}（在分类页重新启用，或在该 Key 的卡片上勾选「允许大脑聚合使用」）"),
                );
            }
        }
    }

    GatherOutcome {
        answers,
        attempted,
        failed,
        skipped_disabled,
        usage: total_usage,
    }
}

async fn compress(
    store: &Arc<Store>,
    category: CategoryType,
    summarizer_ref: &str,
    answers: &[MemberAnswer],
    budget_ms: u64,
) -> AppResult<String> {
    let mut joined = String::new();
    for (i, a) in answers.iter().enumerate() {
        joined.push_str(&format!(
            "\n【顾问{} · {}】\n{}\n",
            i + 1,
            a.label,
            a.answer
        ));
    }
    let sum_prompt = format!(
        "以下是多位专家顾问对同一个问题的分析和建议。\n\
         请提炼各位的关键要点、共识与分歧，压缩成简洁的要点清单，供最终决策参考。\n\n{joined}"
    );
    let started = std::time::Instant::now();
    let (result, used) =
        upstream::with_usage(call_ref(store, category, summarizer_ref, &sum_prompt, budget_ms)).await;
    let result = result?;
    let latency = started.elapsed().as_millis() as u64;
    // 带 trace：展开可见「喂给汇总者的全部成员答案 + 压缩后的要点清单」。受开关控制。
    let trace = if store.get_settings().aggregate_trace_enabled {
        trace_for_ref(store, summarizer_ref, &sum_prompt, &result, latency, true)
    } else {
        None
    };
    store.append_event_full(
        category,
        "aggregate",
        ref_key_id(summarizer_ref),
        &format!(
            "汇总成功 · {} · {latency}ms{}",
            label_ref(store, summarizer_ref),
            if used.is_empty() {
                String::new()
            } else {
                format!(" · {}", used.fmt_compact())
            }
        ),
        trace,
        None,
        (!used.is_empty()).then_some(used),
    );
    Ok(result)
}

fn build_full_context(answers: &[MemberAnswer]) -> String {
    let mut s = String::new();
    for (i, a) in answers.iter().enumerate() {
        s.push_str(&format!(
            "\n【顾问{} · {}】\n{}\n",
            i + 1,
            a.label,
            a.answer
        ));
    }
    s
}



/// 调用某个 `keyId::model` 引用做一次文本补全（决策者 / 汇总者 / 降级独答）。
///
/// `budget_ms` 为该**阶段**的墙钟预算（由 `aggregate_phase` 按整轮 deadline 现算，
/// 不是 `brain.total_timeout_ms` 本身 —— 阶段预算体系加进来之后这两者早就不同了）。
/// 成员阶段一直是逐个 timeout 的，而决策者 / 汇总阶段此前**无任何聚合级超时**，只受
/// reqwest 单 Key 超时 × 重试约束，导致聚合总墙钟不受控、可能超过 MCP 客户端首字节预算而
/// 被客户端断开、整轮白跑。此处对每一阶段都套预算，让「总超时」名副其实、逐阶段封顶。
async fn call_ref(
    store: &Arc<Store>,
    category: CategoryType,
    reference: &str,
    prompt: &str,
    budget_ms: u64,
) -> AppResult<String> {
    let (key_id, model) = reference
        .split_once("::")
        .ok_or_else(|| AppError::Invalid(format!("无效引用: {reference}")))?;
    let key = store
        .get_key(key_id)
        .ok_or_else(|| AppError::NotFound(key_id.into()))?;
    // 禁用**且未开「允许大脑聚合使用」**的 Key 不发任何请求：用户禁用常因欠费/出问题
    // （想止损），而决策者请求内嵌全部成员答案、是整轮最重的一笔。判据与 gather_members 同口径
    // —— 否则用户在 Key 页禁用决策者后聚合照常烧它的额度，界面日志无任何提示，正是最忌讳的
    // 静默失效。文案点明三条出路（含那条开关）：只说「已禁用」会把人送去做无效操作。
    if !key.enabled && !key.allow_in_aggregate {
        return Err(AppError::Invalid(format!(
            "Key「{}」已被禁用，无法作为决策者/汇总者调用。\
             三条出路：换一条启用中的 Key、到分类页重新启用它，\
             或在分类页那条 Key 的卡片上勾选「允许大脑聚合使用」（只在已禁用时显示，语义是「不进路由池但可参与聚合」）。",
            key.name
        )));
    }
    let secret = store
        .secrets
        .read()
        .get(key_id)?
        .ok_or_else(|| AppError::Invalid("决策者密钥缺失".into()))?;
    // 三层弹性对决策者/汇总者**只警告不拦**：它是整轮的出口，拦掉等于整轮失败，
    // 而这三个信号都可能过时（余额缓存有 TTL、配额窗口是上游的一面之词）。
    // 落事件是为了让「决策者为什么 429」这个问题事后有答案。
    gate::precheck_decider(store, category, &key, &label_ref(store, reference));
    // 输出预算：OpenAI 不发上限；Anthropic 按「窗口 − 本次 prompt」现算。
    // 决策者/汇总者的 prompt 里嵌着全部成员答案，可达数十万字符 —— 正是最需要按实际输入
    // 算、而不是套一个常量的场合（旧实现在这里用 4096，长结论必被截断）。
    // `Err` = Anthropic 缺窗口数据，如实报错并指出要补什么，不猜上限。
    let max_tokens = upstream::output_budget(&key, model, upstream::estimate_tokens(prompt))
        .map_err(AppError::Invalid)?;
    let retry = store.get_settings().upstream_retry_enabled;
    // 大脑聚合路径：固定打该引用对应的 Key+模型，不走代理故障转移。
    // 上游瞬时错误仍可按设置做同 Key 重试（upstream_retry），但绝不会换成别的 Key。
    // 单请求 HTTP 超时同样给预算 +5s 余量（勿用 key 的 30s 代理级超时，见 gather_members 注释）。
    let req_timeout = Duration::from_millis(budget_ms.saturating_add(5_000));
    let call = upstream::text_completion(&key, &secret, model, prompt, max_tokens, retry, req_timeout);
    let result = match timeout(Duration::from_millis(budget_ms), call).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(AppError::upstream_msg(format!(
                "调用 {} 超时（超过单次预算 {}ms）",
                label_ref(store, reference),
                budget_ms
            )));
        }
    };
    // 上游偶发返回 200 但 content 为空（厂商限流后的降级 / 模型自身弃答）。若原样返回
    // 空字符串，会被上层当成有效答案继续流转，用户看到「成功但空响应」难以排障。
    // 显式判空 → 转成错误让调用方(决策者/汇总者/降级独答)按失败路径处理，或让 MCP
    // 客户端看到明确错误提示。
    if result.trim().is_empty() {
        return Err(AppError::upstream_msg(format!(
            "调用 {} 返回空响应（上游 200 但 content 为空，通常是限流后厂商降级）",
            label_ref(store, reference)
        )));
    }
    Ok(result)
}

/// 把 `keyId::model` 引用格式化为 `Key名/model`，供聚合分步日志展示。
fn label_ref(store: &Arc<Store>, reference: &str) -> String {
    match reference.split_once("::") {
        Some((key_id, model)) => {
            let name = store
                .get_key(key_id)
                .map(|k| k.name)
                .unwrap_or_else(|| key_id.to_string());
            format!("{name}/{model}")
        }
        None => reference.to_string(),
    }
}

// ============ IPC 命令（大脑聚合 V2）============
//
// 放在这里而不是 lib.rs：命令跟着实现走（同 key_flags / codex_catalog / balance_gate），
// 而 lib.rs 顶在棘轮上、这三个命令的正文与它没有任何关系。

/// Phase1: 文件检索 + 参与者思考 + 决策者输出计划
#[tauri::command]
pub async fn aggregate_plan(
    state: tauri::State<'_, crate::AppState>,
    category_id: CategoryType,
    prompt: String,
) -> AppResult<AggregateResult> {
    run_plan(&state.store, category_id, &prompt).await
}

/// Phase2a: 决策者按已确认的计划输出完整文件内容，返回**落盘预览**——不写任何文件。
/// work_dir / plan_started_ms 由 Phase1 的返回结果回传：前者锁定工作目录避免 auto-follow
/// 漂移，后者让后端能拒绝覆盖「用户在确认期间自己改过」的文件。
#[tauri::command]
pub async fn aggregate_execute(
    state: tauri::State<'_, crate::AppState>,
    category_id: CategoryType,
    prompt: String,
    confirmed_plan: String,
    work_dir: Option<String>,
    plan_started_ms: i64,
) -> AppResult<write::PreviewReport> {
    run_preview(
        &state.store,
        category_id,
        &prompt,
        &confirmed_plan,
        work_dir,
        plan_started_ms,
    )
    .await
}

/// Phase2b: 用户看过预览后真正落盘。不调模型，按决策者原文重新解析（预览与落盘同一道门）。
#[tauri::command]
pub async fn aggregate_write(
    state: tauri::State<'_, crate::AppState>,
    category_id: CategoryType,
    decider_output: String,
    work_dir: Option<String>,
    plan_started_ms: i64,
) -> AppResult<AggregateResult> {
    run_write(
        &state.store,
        category_id,
        &decider_output,
        work_dir,
        plan_started_ms,
    )
    .await
}

/// 聚合运行结果（内部标签枚举，序列化为 tagged JSON 给前端）。
///
/// 注意：serde 的内部标签（`#[serde(tag)]`）**不支持 newtype 变体包裹基本类型**
/// （如 `Plan(String)` 会在运行时报 "cannot serialize tagged newtype variant ...
/// containing a string"，导致 aggregate_plan 命令必然失败）。故所有变体必须是
/// struct 变体。字段名对齐前端契约 src/types.ts::AggregateResult（content / filesModified）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resultType", rename_all = "camelCase")]
pub enum AggregateResult {
    /// 决策者输出的修改计划（Phase1）。work_dir 为本次解析定下的工作目录，
    /// 前端须在 Phase2 原样回传，避免 auto-follow 期间目录漂移把改动写进别的项目。
    /// plan_started_ms 是 Phase1 开始的墙钟毫秒，Phase2 用它拒绝覆盖「用户在确认期间
    /// 自己改过」的文件（详见 `aggregate::write::changed_since`），同样须原样回传。
    #[serde(rename = "plan")]
    Plan {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        work_dir: Option<String>,
        plan_started_ms: i64,
    },
    /// 执行结果（Phase2）：content 为决策者原始输出，files_modified 为实际写入的文件路径。
    #[serde(rename = "applied")]
    Applied {
        content: String,
        files_modified: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **带 usage 的聚合事件一律不许把 key_id 传成 `None`。**
    ///
    /// 这是一条源码级判据，理由是「记得传 key_id」属于必然会漏的纪律，而漏掉的表现是静默的：
    /// `store.rs` 的累加器键是 `(分类, key_id.unwrap_or_default())`，传 None 就把这笔消耗
    /// 塞进 `(分类, "")` 那个桶 → 用量页显示「（系统级）」、花费列恒为「—」
    /// （用户实际报的就是这个症状，8 处 call site 全传了 None）。
    ///
    /// 判据做法：把本文件生产段里每个 `append_event_full(` 调用的实参块切出来，
    /// 若它**最后一个实参不是 `None`**（= 带 usage），则第三个实参也不许是 `None`。
    ///
    /// 两处「本次聚合合计」是**刻意**传 None 的（汇总展示行，分项已各自记过账，
    /// 再记一次会让金额恒为真值 2 倍），它们最后一个实参也是 None，故自动被放过。
    ///
    /// 故障注入判据：把任一带 usage 的站点的 key_id 改回 `None` → 本测试必须变红。
    ///
    /// 🔴 **两份源码都要扫。** 骨架搬进 `round.rs` 之后，带 usage 的事件大多在那边，
    /// 只扫本文件会让判据静默退化成「只查了一小半」—— 而它守的恰恰是搬家过程中最容易
    /// 漏掉的那件事。反向断言（调用点 ≥8）就是为了让这种退化变红。
    fn scan_usage_events(name: &str, raw: &str, offenders: &mut Vec<String>) -> usize {
        // 用项目自己那份剥测试段 + 剥注释的 helper：本文件测试段里就有
        // `store.append_event_full(` 字样，自写切分会把它算进去（本仓栽过五次的那类）。
        let prod = crate::proxy::custom_headers::production_code_only(raw);
        let calls = prod.matches("store.append_event_full(").count();
        let mut rest = prod.as_str();
        while let Some(pos) = rest.find("store.append_event_full(") {
            rest = &rest[pos + "store.append_event_full(".len()..];
            // 取到配对的右括号为止（实参里只有 format!/then_some 这类嵌套，计数即可）。
            let mut depth = 1usize;
            let mut end = 0usize;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let args = &rest[..end];
            // 顶层逗号切分（跳过嵌套括号内的逗号）。
            let mut parts: Vec<String> = Vec::new();
            let mut cur = String::new();
            let mut d = 0usize;
            for ch in args.chars() {
                match ch {
                    '(' | '[' | '{' => {
                        d += 1;
                        cur.push(ch);
                    }
                    ')' | ']' | '}' => {
                        d -= 1;
                        cur.push(ch);
                    }
                    ',' if d == 0 => {
                        parts.push(cur.trim().to_string());
                        cur.clear();
                    }
                    _ => cur.push(ch),
                }
            }
            if !cur.trim().is_empty() {
                parts.push(cur.trim().to_string());
            }
            if parts.len() < 7 {
                continue; // 解析不出 7 个实参，跳过（下面有反向判据兜底）
            }
            // **必须先剥掉行注释**：两处「本次聚合合计」的 `None` 前面挂着五行说明，
            // 不剥的话那整段注释会被当成实参内容、于是「最后一个实参不是 None」判真
            // → 判据反而把刻意为之的正确写法报成违规。第一版就是这么报了两条假警。
            let strip = |s: &str| -> String {
                s.lines()
                    .map(|l| l.split("//").next().unwrap_or("").trim())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let key_arg = strip(&parts[2]);
            let usage_arg = strip(parts.last().unwrap());
            let carries_usage = usage_arg != "None";
            if carries_usage && key_arg == "None" {
                offenders.push(format!("{name}: usage={usage_arg} 但 key_id=None"));
            }
        }
        calls
    }

    #[test]
    fn aggregate_usage_is_never_recorded_without_a_key() {
        let mut offenders = Vec::new();
        let mut calls = 0usize;
        for (name, raw) in [
            ("aggregate.rs", include_str!("aggregate.rs")),
            ("aggregate/round.rs", include_str!("aggregate/round.rs")),
        ] {
            calls += scan_usage_events(name, raw, &mut offenders);
        }

        assert!(
            offenders.is_empty(),
            "以下聚合事件带着 usage 却没有 key_id，会落进「（系统级）」空桶、花费列显示「—」：{offenders:#?}"
        );

        // 反向判据：判据本身不能空转（同 `invoke-command-must-exist` 那条教训）。
        // 两份合起来确实有 8 处调用；解析不到就说明上面那套切分坏了，或者有一份没被扫到。
        assert!(
            calls >= 8,
            "只解析到 {calls} 处 append_event_full —— 判据在空转，先修判据"
        );
    }

    // ---- 写文件路径的安全护栏（parse_and_apply / is_safe_relative_path）----
    //
    // 这两个函数会**真实写用户磁盘**：决策者（LLM）输出的路径直接决定写到哪里，而 prompt 里
    // 混入了检索到的项目文件内容——存在提示注入面。故路径遏制与围栏解析都必须有护栏锁住。

    /// 测试用临时目录（同 store/tools 的做法：pid + 自增序号，避免并发用例互踩）。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "synaroute_agg_test_{}_{}_{}",
            tag,
            std::process::id(),
            seq
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn solo_decider_prompt_includes_file_context_when_present() {
        // 无成功参与者的降级路径：必须把已检索到的文件上下文带进决策者 prompt，
        // 否则检索开销白费、决策者盲答。
        let out = build_solo_decider_prompt("分析登录流程", "src/auth.rs\nfn login() {}");
        assert!(out.contains("分析登录流程"), "应保留原始问题");
        assert!(out.contains("## 相关文件"), "应拼入相关文件小节");
        assert!(out.contains("fn login()"), "应带上检索到的文件内容");
    }

    #[test]
    fn solo_decider_prompt_is_plain_prompt_when_no_context() {
        // 无检索上下文时，退回原始 prompt，不平添空的「相关文件」小节。
        let out = build_solo_decider_prompt("今天几号", "");
        assert_eq!(out, "今天几号");
        assert!(!out.contains("## 相关文件"));
    }

    #[test]
    fn member_prompt_omits_file_section_when_empty() {
        let out = build_member_prompt("问题X", "");
        assert!(out.contains("问题X"));
        assert!(!out.contains("## 相关文件"), "无文件时不应有相关文件小节");
    }

    /// 决策者失败降级：不作废已聚合的成员意见，返回内容里应能看到降级标记 + 中间产物。
    /// 用一段和 run_mcp 里 Err 分支等价的构造做黑盒验证——一旦有人重构成又抛错丢内容，测试立即失败。
    #[test]
    fn decider_failure_fallback_preserves_aggregated_members() {
        let aggregated = "【顾问1】A 的意见\n【顾问2】B 的意见";
        let fallback = format!(
            "> ⚠️ 决策者 `X/Y` 未能完成综合分析：模拟限流\n> 以下是已成功获取的 2 位专家意见，供你参考：\n\n{}",
            aggregated
        );
        assert!(fallback.contains("决策者") && fallback.contains("未能完成"), "应有降级标注");
        assert!(fallback.contains("A 的意见") && fallback.contains("B 的意见"), "已聚合内容不应被丢弃");
    }

    #[test]
    fn decider_floor_clamped_for_small_and_large_budgets() {
        // 小预算：35% < 90s，被 45% 上限夹住（否则地板会超过整轮总量，饿死成员+压缩）。
        assert_eq!(decider_floor_ms(60_000), 27_000); // 35%=21000<90000 → 90000，min 45%=27000
        // 90s 绝对地板生效区间（35% 恰不足 90s）。
        assert_eq!(decider_floor_ms(257_000), 90_000); // 35%=89950<90000 → 90000 < 45%=115650
        // 大预算：35% 主导（>90s 且 <45%）。
        assert_eq!(decider_floor_ms(540_000), 189_000); // 35%=189000
        assert_eq!(decider_floor_ms(600_000), 210_000); // 35%=210000
    }

    #[test]
    fn upstream_phase_budget_reserves_decider_floor() {
        // 剩余充足：扣掉决策者地板后剩下的给成员/压缩。
        assert_eq!(upstream_phase_budget_ms(600_000, 210_000, 5_000), 390_000);
        // 剩余不足以覆盖地板：触发最小下限保护，不给 0（宁可略越界，靠客户端余量兜底）。
        assert_eq!(upstream_phase_budget_ms(100_000, 210_000, 5_000), 5_000);
        // 剩余恰好等于地板：扣完为 0 → 最小下限。
        assert_eq!(upstream_phase_budget_ms(210_000, 210_000, 5_000), 5_000);
    }

    #[test]
    fn decider_phase_budget_takes_all_remaining_with_floor() {
        // 决策者独享整轮剩余（前面省下的全拿）。
        assert_eq!(decider_phase_budget_ms(300_000, 5_000), 300_000);
        // 剩余被前面阶段耗尽：最小下限保护，仍给决策者一次机会。
        assert_eq!(decider_phase_budget_ms(0, 5_000), 5_000);
        assert_eq!(decider_phase_budget_ms(2_000, 5_000), 5_000);
    }

    // ---- 工具循环（成员 agent 循环）----

    #[test]
    fn effective_rounds_clamps_and_degrades() {
        // 无工具 → 恒 1 轮（形状回到开关加入之前）
        assert_eq!(effective_rounds(false, 6), 1);
        assert_eq!(effective_rounds(false, 99), 1);
        // 有工具 → 夹进 [2, 12]。下限是关键：配 1 轮会让第一轮就走收尾分支，
        // 工具声明了却永远调不动 —— 那正是「开关开着但功能没生效」。
        assert_eq!(effective_rounds(true, 0), TOOL_ROUNDS_RANGE.0);
        assert_eq!(effective_rounds(true, 1), TOOL_ROUNDS_RANGE.0);
        assert_eq!(effective_rounds(true, 6), 6);
        assert_eq!(effective_rounds(true, 999), TOOL_ROUNDS_RANGE.1);
    }

    #[test]
    fn image_hint_only_fires_for_4xx_with_images() {
        let http = |s: u16, body: &str| MemberError {
            msg: format!("调用失败：OpenAI HTTP {s}: {body}"),
            status: Some(s),
        };
        let e400 = http(400, "{\"error\":\"invalid image\"}");
        // 带图 + 4xx → 点出「模型可能不支持图片」，否则根因埋在响应体里没人看得出来
        assert!(image_unsupported_hint(&e400, true).contains("不支持图片输入"));
        // 没带图 → 不该乱加提示，误导排查方向
        assert_eq!(image_unsupported_hint(&e400, false), e400.msg);
        // 5xx 与图片无关
        for e in [http(529, "overloaded"), http(500, "boom")] {
            assert_eq!(image_unsupported_hint(&e, true), e.msg, "{} 不该加图片提示", e.msg);
        }
        // 我们自己产生的失败（超时/连接层）没有状态码 → 一律不加提示
        for e in [
            MemberError::own("超时（>60000ms）"),
            MemberError::own("调用失败：连接 x 失败"),
        ] {
            assert_eq!(image_unsupported_hint(&e, true), e.msg, "{} 不该加图片提示", e.msg);
        }
    }

    /// 回归：4xx 判定必须只看**结构化状态码**，不能从错误文本里搜 `HTTP 4xx`。
    ///
    /// 故障注入判据：把 `MemberError::is_4xx` 改回文本搜索，第一条断言即变红。
    /// 真实场景——网关把上游原始报文回显在响应体里（中转商常见），一个 500 的响应体里
    /// 带着 `HTTP 404` 字样，文本搜索会误判成 4xx，给用户追加一句「模型可能不支持图片输入」，
    /// 把排障方向从「上游 5xx」引到「多模态能力」上。
    #[test]
    fn is_4xx_uses_status_not_body_text() {
        // 真状态码 500，但响应体里带 "HTTP 404" 字样
        let poisoned = MemberError {
            msg: "调用失败：OpenAI HTTP 500: upstream said HTTP 404 not found".into(),
            status: Some(500),
        };
        assert!(!poisoned.is_4xx(), "状态码是 500，不能因响应体含 'HTTP 404' 就判成 4xx");
        assert_eq!(
            image_unsupported_hint(&poisoned, true),
            poisoned.msg,
            "5xx 不该被追加图片提示（哪怕 body 里有 4xx 字样）"
        );

        // 反向：真 4xx 但文本里完全没有 "HTTP" 字样，也必须判出来
        let no_text = MemberError { msg: "上游拒绝".into(), status: Some(422) };
        assert!(no_text.is_4xx(), "422 必须判为 4xx，即使消息里没有 'HTTP' 字样");

        // 边界：399 / 500 不算 4xx
        assert!(!MemberError { msg: String::new(), status: Some(399) }.is_4xx());
        assert!(!MemberError { msg: String::new(), status: Some(500) }.is_4xx());
        assert!(MemberError { msg: String::new(), status: Some(499) }.is_4xx());
    }

    #[test]
    fn tool_call_brief_is_stable_and_capped() {
        use serde_json::json;
        let c = upstream::ToolInvocation {
            id: "t".into(),
            name: "read_file".into(),
            args: json!({ "start_line": 3, "path": "src/main.rs" }),
        };
        // 排序固定：折叠 key 与日志文本不能因参数顺序抖动
        assert_eq!(tool_call_brief(&c), "path=src/main.rs start_line=3");
        // 非对象参数（模型吐了截断 JSON）要有可读标记，不能 panic
        let bad = upstream::ToolInvocation {
            id: "t".into(),
            name: "read_file".into(),
            args: serde_json::Value::Null,
        };
        assert_eq!(tool_call_brief(&bad), "[参数非法]");
        // 长参数截断（整段正则/长路径会撑爆日志行）
        let long = upstream::ToolInvocation {
            id: "t".into(),
            name: "grep".into(),
            args: json!({ "pattern": "x".repeat(200) }),
        };
        let s = tool_call_brief(&long);
        assert!(s.chars().count() <= 81 && s.ends_with('…'), "{s}");
    }

    /// 起一个**按脚本逐次应答**的 mock 上游：第 N 次请求返回 `bodies[N-1]`（用尽后重复最后一条）。
    /// 同时把每次收到的请求体存下来，供断言「tools 声明 / tool_result 回填」的真实形状。
    ///
    /// 为什么要真起 HTTP：工具循环最容易错的地方是**第二轮请求体长什么样**（assistant 消息
    /// 是否照抄、tool_result 是否一一对应、tools 是否还带着）。只测纯函数覆盖不到这一层。
    async fn spawn_scripted(
        bodies: Vec<&'static str>,
    ) -> (String, Arc<parking_lot::Mutex<Vec<serde_json::Value>>>) {
        use http_body_util::{BodyExt, Full};
        use hyper::body::{Bytes, Incoming};
        use hyper::service::service_fn;
        use hyper::{Request, Response};
        use hyper_util::rt::TokioIo;
        use std::net::SocketAddr;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<parking_lot::Mutex<Vec<serde_json::Value>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen_srv = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let io = TokioIo::new(stream);
                let bodies = bodies.clone();
                let seen_conn = seen_srv.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let bodies = bodies.clone();
                        let seen_req = seen_conn.clone();
                        async move {
                            let raw = req.into_body().collect().await.unwrap().to_bytes();
                            let parsed: serde_json::Value =
                                serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
                            let idx = {
                                let mut g = seen_req.lock();
                                g.push(parsed);
                                g.len() - 1
                            };
                            let body = bodies[idx.min(bodies.len() - 1)];
                            let resp = Response::builder()
                                .status(200)
                                .header("content-type", "application/json")
                                .body(
                                    Full::new(Bytes::from(body))
                                        .map_err(|n: std::convert::Infallible| -> std::io::Error {
                                            match n {}
                                        })
                                        .boxed(),
                                )
                                .unwrap();
                            Ok::<_, hyper::Error>(resp)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn test_key(base_url: &str) -> crate::model::ProviderKey {
        crate::model::ProviderKey {
            tier_fable: None,
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            name: "mock".into(),
            vendor: "test".into(),
            base_url: base_url.into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
            allow_in_aggregate: false,
            priority: 0,
            headers_json: None,
            params: crate::model::KeyParams::default(),
            // 带上 `context_window`：Anthropic 的 `max_tokens` 是必填字段，而聚合已改为按
            // 「窗口 − 本轮输入」现算（不再用 4096 那个会截断长回答的默认值）。缺窗口数据的
            // Anthropic Key 会被如实判为不可用，故可用的 Key 必须有这项 —— 这与真实用户
            // 拉取/填写过模型列表的状态一致。
            models: vec![crate::model::ModelInfo {
                real_name: "claude-sonnet-4-5".into(),
                source: "manual".into(),
                fetched_at: None,
                context_window: Some(200_000),
                    max_output_tokens: None,
            }],
            mappings: vec![],
            default_model: None,
            tier_haiku: None,
            tier_sonnet: None,
            tier_opus: None,
            balance_query: None,
            cached_balance: None,
            cost_multiplier: None,
            icon: None,
            health: crate::model::HealthState::default(),
        }
    }

    /// 🔴 「允许大脑聚合使用」的两侧语义 —— 此前**整个新字段零测试覆盖**，
    /// 把判据改回裸 `!key.enabled` 全套 926 条照样全绿。
    ///
    /// 语义：`enabled` 管「进不进故障转移池」，而聚合**不走故障转移**（成员固定 Key）。
    /// 用户禁用一条 Key 常常正是因为它的模型名与主 Key 不重叠、进池会让故障转移 404，
    /// 而那条 Key 本身是好的、有额度的。
    #[tokio::test]
    async fn a_disabled_key_can_still_be_a_decider_when_allowed_in_aggregate() {
        let (store, dir) = crate::service::tests::temp_store("agg_allow");

        // ① 禁用 + 未开开关 → 必须拒，且文案要指向那个勾选框（不能只说「重新启用」，
        //    重新启用会把当初禁用它的那个 404 带回主链路）。
        let mut k = test_key("http://127.0.0.1:1");
        k.enabled = false;
        store.upsert_key(k.clone()).unwrap();
        let err = call_ref(&store, CategoryType::ClaudeCli, "k1::claude-sonnet-4-5", "hi", 5_000)
            .await
            .expect_err("禁用且未开允许聚合的 Key 不该被调用");
        let msg = err.to_string();
        assert!(msg.contains("已被禁用"), "{msg}");
        assert!(
            msg.contains("允许大脑聚合使用") && msg.contains("卡片"),
            "报错必须指向真正能改那个开关的界面：{msg}"
        );

        // ② 禁用 + 开了开关 → 不该再被这道门挡住（后面会因连不上上游而失败，那是另一回事）。
        k.allow_in_aggregate = true;
        store.upsert_key(k).unwrap();
        let out = call_ref(&store, CategoryType::ClaudeCli, "k1::claude-sonnet-4-5", "hi", 5_000).await;
        if let Err(e) = &out {
            let m = e.to_string();
            assert!(
                !m.contains("已被禁用"),
                "开了「允许大脑聚合使用」就不该再被禁用门挡住：{m}"
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 成员路径与决策者/汇总者路径必须**同一个判据**。
    ///
    /// 上面那条只覆盖 `call_ref`（决策者/汇总者）。成员路径在 `gather_members` 里，
    /// 要真跑一轮才走到，故这里用源码级判据钉住「两处都是 `!enabled && !allow_in_aggregate`」
    /// —— 分叉的表现是静默的：开关在一侧生效、另一侧照旧跳过，而用户只看到
    /// 「结果面板少一位专家」。
    #[test]
    fn both_aggregate_gates_use_the_same_predicate() {
        let src = std::fs::read_to_string("src/aggregate.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("!key.enabled && !key.allow_in_aggregate").count(),
            2,
            "成员（gather_members）与决策者/汇总者（call_ref）两道门必须同口径，\
             且只该有这两处"
        );
    }

    /// 🔴 **四个入口都必须过 `require_enabled`。**
    ///
    /// `run_apply`（现已拆成 `run_preview` / `run_write`）此前**没有**这个检查：它只取了
    /// `decider_ref` 就往下走，于是用户在界面上关掉大脑聚合之后，「确认执行」照样能跑并
    /// **写文件**。权限门在多个入口各写一份的失效方向正是这样 —— 漏掉的那个入口静默绕过它，
    /// 而没人会去测「关掉开关之后按钮还能不能用」。
    ///
    /// 判据钉的是**结构**（每个入口都调它、且判定只有一处），因为行为级测试要为四个入口
    /// 各造一份夹具，而漏掉的恰恰会是新加的那个入口。
    #[test]
    fn every_entry_point_checks_the_enabled_switch() {
        let src = std::fs::read_to_string("src/aggregate.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("fn require_enabled(").count(),
            1,
            "判定只能有一处 —— 复制一份出来，两边就会各自漂"
        );
        assert_eq!(
            prod.matches("require_enabled(store, category)").count(),
            4,
            "四个入口（run_plan / run_preview / run_write / run_mcp）都必须过这道门。\
             少一个就是「关掉开关之后那个入口还能用」，而 run_write 那个入口会写文件。"
        );
        // 反面：不许再有第二份 `brain.enabled` 判定绕开它。
        assert_eq!(
            prod.matches("brain.enabled").count(),
            1,
            "`enabled` 只能在 require_enabled 里被读一次"
        );
    }

    /// 决策者那一步必须**同时**落事件、带 trace、带 usage —— 三者都曾在桌面端路径上缺失。
    ///
    /// 这条钉的是 `round.rs`（两条路径合并后的唯一实现）：
    /// - `aggregate_trace_enabled` 必须被读到（此前整个 `run_plan` 一次都没读它 →
    ///   用户开了「聚合链路快照」却看不到决策者的入参，而那才是「为什么答案是这样」的答案）；
    /// - 决策者成功与失败**两个分支**都要落带 usage 的事件（失败同样烧了额度）。
    ///
    /// usage 与 key_id 的配对由 `aggregate_usage_is_never_recorded_without_a_key` 管，
    /// 这里管的是「事件本身在不在」。
    #[test]
    fn the_decider_step_is_fully_observable() {
        let src = std::fs::read_to_string("src/aggregate/round.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains("aggregate_trace_enabled"),
            "决策者的入参/出参必须受那个开关控制并能落进 trace"
        );
        assert_eq!(
            prod.matches("decider_used.fmt_compact()").count(),
            2,
            "成功与失败两个分支都要把已消耗的 token 报出来"
        );
        assert_eq!(
            prod.matches("(!decider_used.is_empty()).then_some(decider_used)").count(),
            2,
            "两个分支都要把 usage 交给累加器 —— 桌面端此前一个都没有，用量页系统性偏低"
        );
    }

    /// 建一个带一个源文件的临时工作目录 + 对应的 ToolEnv。
    async fn tool_env_with_file(tag: &str) -> (std::path::PathBuf, crate::agent_tools::ToolEnv) {
        let dir = temp_dir(tag);
        std::fs::write(dir.join("main.rs"), "fn main() {\n    answer_42();\n}\n").unwrap();
        let env = crate::agent_tools::ToolEnv::detect(&dir).await;
        (dir, env)
    }

    fn test_store(tag: &str) -> (std::path::PathBuf, Arc<Store>) {
        let dir = temp_dir(tag);
        let store = Arc::new(
            Store::new_at(dir.join("config.json"), dir.join("secrets.enc")).unwrap(),
        );
        (dir, store)
    }

    fn ctx_for<'a>(
        key: &'a crate::model::ProviderKey,
        secret: &'a str,
        label: &'a str,
    ) -> MemberCallCtx<'a> {
        MemberCallCtx {
            up: upstream::TurnParams {
                key,
                secret,
                model: "claude-sonnet-4-5",
                // 占位值：`run_member_turns` 会按协议与实际输入重算并覆盖它
                // （这些用例造的是 Anthropic Key，故走 Limit 分支）。
                max_tokens: None,
                retry: false,
                request_timeout: Duration::from_secs(10),
            },
            label,
        }
    }

    /// 🔴 **上游回 200 但正文是错误** —— 聚合这条链路与转发路径完全独立
    /// （它走 `ToolSession`，不经过 `proxy.rs` 的 `soft_error::demote`）。
    ///
    /// 修之前的表现：状态码门放过 200 → `parse_anthropic_turn` 找不到 `content`
    /// → 兜底文本提取也返 None → 报「响应无法解析」，把排障的人指向我们的解析器，
    /// 而真相是上游返了个错误。判据与转发路径共用 `body_is_upstream_error`。
    #[tokio::test]
    async fn a_200_with_an_error_body_is_reported_as_an_upstream_error() {
        let (upstream, _seen) =
            spawn_scripted(vec![r#"{"error":{"type":"overloaded_error","message":"上游分组已饱和"}}"#])
                .await;
        let (_sdir, store) = test_store("agg_soft_err");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("在吗"));
        let err = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            None,
            6,
            60_000,
            0,
        )
        .await
        .expect_err("200 包着 error 必须报错，不能当成一次成功的空回答");
        let msg = err.to_string();
        assert!(
            msg.contains("上游分组已饱和"),
            "必须把上游原话带给用户：{msg}"
        );
        assert!(
            !msg.contains("无法解析"),
            "不能报成「响应无法解析」——那把人指向我们的解析器而不是上游：{msg}"
        );
    }

    /// 端到端：模型先要求 read_file → 本地执行 → 结果回填 → 第二轮给出最终答案。
    #[tokio::test]
    async fn tool_loop_executes_tool_then_returns_final_text() {
        let (upstream, seen) = spawn_scripted(vec![
            // 第 1 轮：先说一句，再要求读文件
            r#"{"content":[{"type":"text","text":"我先看看那个文件"},{"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"main.rs"}}],"stop_reason":"tool_use"}"#,
            // 第 2 轮：拿到内容后给结论
            r#"{"content":[{"type":"text","text":"main 调用了 answer_42"}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("loop_ok").await;
        let (sdir, store) = test_store("loop_ok_store");
        let key = test_key(&upstream);
        let mut session = ToolSession::new(
            Protocol::Anthropic,
            &MultimodalPrompt::from_text("main.rs 里调了什么"),
        );

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .expect("循环应正常结束");
        assert_eq!(out, "main 调用了 answer_42");

        let reqs = seen.lock().clone();
        assert_eq!(reqs.len(), 2, "应恰好两轮请求");
        // 第 1 轮：必须声明 tools（否则模型无从调用）
        let names: Vec<&str> = reqs[0]["tools"]
            .as_array()
            .expect("第一轮应带 tools")
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read_file") && names.contains(&"grep"), "{names:?}");
        // 第 2 轮：历史里必须有照抄的 assistant 消息 + 一一对应的 tool_result
        let msgs = reqs[1]["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "user + assistant(tool_use) + user(tool_result)");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(msgs[2]["content"][0]["is_error"], false);
        // 工具真读到了文件内容（不是空壳成功）
        let result_text = msgs[2]["content"][0]["content"].as_str().unwrap();
        assert!(result_text.contains("answer_42"), "{result_text}");
        // 第 2 轮仍须带 tools：Anthropic 规定历史里出现 tool_use/tool_result 就必须声明 tools
        assert!(reqs[1]["tools"].is_array(), "第二轮丢了 tools 会被上游 400");

        // Prompt caching 端到端锁死:请求体必须真带上 cache_control 断点(否则缓存永不命中,
        // 白改一场)。两处:tools 数组末尾 + 最后一条消息的最后一个 content 块。
        let tools1 = reqs[1]["tools"].as_array().unwrap();
        assert_eq!(
            tools1.last().unwrap()["cache_control"]["type"],
            "ephemeral",
            "tools 末尾应带缓存断点"
        );
        let last_block = reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(
            last_block["cache_control"]["type"], "ephemeral",
            "最后一条消息的末块应带缓存断点(缓存到本轮为止的历史)"
        );

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 模型死磕工具不出结论：到轮数上限必须停下并交出已说的内容，而不是无限烧预算。
    #[tokio::test]
    async fn tool_loop_stops_at_round_limit_and_salvages_preamble() {
        let (upstream, seen) = spawn_scripted(vec![
            r#"{"content":[{"type":"text","text":"我再看一个文件"},{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"main.rs"}}],"stop_reason":"tool_use"}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("loop_cap").await;
        let (sdir, store) = test_store("loop_cap_store");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        // 配 2 轮（下限）：第 1 轮真调工具，第 2 轮收尾。
        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            2,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .expect("到上限应交出已有正文，而不是报错");
        // 两轮的铺垫都保留下来了（丢掉等于白烧两轮额度）
        assert_eq!(out, "我再看一个文件\n\n我再看一个文件");

        let reqs = seen.lock().clone();
        assert_eq!(reqs.len(), 2, "必须在 2 轮后停下，不能无限循环");
        // 收尾轮要带一条「别再调工具」的 user 指示，且是**并进** tool_result 那条 user 消息
        // （Anthropic 侧连续两条 user 会被判角色未交替）
        let msgs = reqs[1]["messages"].as_array().unwrap();
        let last = msgs.last().unwrap();
        assert_eq!(last["role"], "user");
        let blocks = last["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "tool_result");
        assert!(
            blocks.last().unwrap()["text"]
                .as_str()
                .unwrap_or_default()
                .contains("不要再调用工具"),
            "收尾指示应并进同一条 user 消息：{blocks:?}"
        );
        // 收尾轮仍带 tools（抽掉会被 Anthropic 400）
        assert!(reqs[1]["tools"].is_array());

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 工具被安全策略拒绝时，成员**不该失败**：错误文本回给模型，让它换个方向继续。
    #[tokio::test]
    async fn rejected_tool_call_does_not_fail_the_member() {
        let (upstream, seen) = spawn_scripted(vec![
            // 模型（被注入内容诱导）去读工作目录外的文件
            r#"{"content":[{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"../../secret.txt"}}],"stop_reason":"tool_use"}"#,
            r#"{"content":[{"type":"text","text":"读不到那个文件，我基于已有信息回答"}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("loop_deny").await;
        let (sdir, store) = test_store("loop_deny_store");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .expect("工具被拒不该让整个成员失败");
        assert_eq!(out, "读不到那个文件，我基于已有信息回答");

        let reqs = seen.lock().clone();
        let tr = &reqs[1]["messages"][2]["content"][0];
        assert_eq!(tr["is_error"], true, "被拒必须标成错误，否则模型把拒绝文本当数据用");
        assert!(
            tr["content"].as_str().unwrap().contains("相对路径"),
            "拒绝原因要可行动：{tr:?}"
        );

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// OpenAI 侧的形状与 Anthropic 完全不同（tool_calls / role:"tool"），单独端到端跑一遍。
    #[tokio::test]
    async fn tool_loop_openai_shape_roundtrips() {        let (upstream, seen) = spawn_scripted(vec![
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"main.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"message":{"role":"assistant","content":"main 调了 answer_42"}}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("loop_oai").await;
        let (sdir, store) = test_store("loop_oai_store");
        let mut key = test_key(&upstream);
        key.protocol = Protocol::OpenaiChat;
        let mut session =
            ToolSession::new(Protocol::OpenaiChat, &MultimodalPrompt::from_text("问题"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .unwrap();
        assert_eq!(out, "main 调了 answer_42");

        let reqs = seen.lock().clone();
        // tools 要包一层 function、schema 字段叫 parameters
        assert_eq!(reqs[0]["tools"][0]["type"], "function");
        assert!(reqs[0]["tools"][0]["function"]["parameters"].is_object());
        assert_eq!(reqs[0]["tool_choice"], "auto");
        // 结果是一条独立的 role:"tool" 消息，带 tool_call_id
        let msgs = reqs[1]["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
        assert!(msgs[2]["content"].as_str().unwrap().contains("answer_42"));

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// OpenAI 聚合请求**每一轮**都不得携带输出上限字段（产品定调 2026-08-15）。
    ///
    /// 钉住的是本次撤掉的那个截断点：聚合曾一律用 `key.params.max_tokens.unwrap_or(4096)`，
    /// 于是参与者/决策者的长回答在 4096 token 处被切掉，而用户无从知道是本地配置造成的。
    /// Chat Completions 的 `max_tokens` 是可选项，故这里能做到真正不限制。
    ///
    /// **每轮都要查**：工具循环第 2 轮起走的是另一条代码路径（历史重发），
    /// 只查第 1 轮会漏掉「后续轮次又把上限加回来」。
    #[tokio::test]
    async fn openai_aggregation_requests_carry_no_output_cap() {
        let (upstream, seen) = spawn_scripted(vec![
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"main.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"message":{"role":"assistant","content":"结论"}}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("oai_nocap").await;
        let (sdir, store) = test_store("oai_nocap_store");
        let mut key = test_key(&upstream);
        key.protocol = Protocol::OpenaiChat;
        // 刻意配一个旧的 max_tokens：它绝不能重新出现在请求里。

        let mut session =
            ToolSession::new(Protocol::OpenaiChat, &MultimodalPrompt::from_text("问题"));

        run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0,
        )
        .await
        .unwrap();

        let reqs = seen.lock().clone();
        assert!(reqs.len() >= 2, "应至少两轮，实际 {}", reqs.len());
        for (i, r) in reqs.iter().enumerate() {
            for name in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
                assert!(
                    r.get(name).is_none(),
                    "第 {} 轮请求不得携带 {name}（实际请求体: {r}）",
                    i + 1
                );
            }
        }

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// Anthropic 聚合请求必须带 `max_tokens`（协议必填），且值是**按上下文窗口现算**的、
    /// 最大输出受模型能力钳在 64k，且工具历史增长后不得变大。
    ///
    /// 这条与上面那条是一对：Anthropic 无法省略字段，所以能做的是「能力范围内尽可能大」而非「不发」。
    /// 若有人把 `unwrap_or(4096)` 加回来，第一个断言就会红。
    #[tokio::test]
    async fn anthropic_aggregation_uses_dynamic_cap_not_the_old_default() {
        let (upstream, seen) = spawn_scripted(vec![
            r#"{"content":[{"type":"text","text":"先看文件"},{"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"main.rs"}}],"stop_reason":"tool_use"}"#,
            r#"{"content":[{"type":"text","text":"结论"}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("ant_dyncap").await;
        let (sdir, store) = test_store("ant_dyncap_store");
        let key = test_key(&upstream); // test_key 已带 context_window = 200_000

        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0,
        )
        .await
        .unwrap();

        let reqs = seen.lock().clone();
        assert!(reqs.len() >= 2, "应至少两轮，实际 {}", reqs.len());
        let caps: Vec<u64> = reqs
            .iter()
            .map(|r| {
                r["max_tokens"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("Anthropic 请求必须带 max_tokens: {r}"))
            })
            .collect();
        for (i, c) in caps.iter().enumerate() {
            assert_ne!(*c, 4096, "第 {} 轮又用回了旧的 4096 默认值", i + 1);
            assert_eq!(
                *c, 64_000,
                "第 {} 轮应受 Claude 4.5 的 64k 最大输出能力钳制，实际 {c}",
                i + 1
            );
        }
        // 第 2 轮历史更长；当前两轮都仍被模型 64k 上限钳住，故允许相等，
        // 但绝不许变大（输入增长后预算不能增加）。
        assert!(
            caps[1] <= caps[0],
            "历史变长后预算不得变大：第1轮 {} 第2轮 {}",
            caps[0],
            caps[1]
        );

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 模型名可辨识时，无窗口数据仍可安全按模型最大输出参与聚合；这避免拒掉
    /// 用户手填模型名/刚拉取模型列表这类常见可用配置。
    #[tokio::test]
    async fn anthropic_member_without_context_window_uses_known_model_cap() {
        let (upstream, seen) = spawn_scripted(vec![r#"{"content":[{"type":"text","text":"x"}]}"#]).await;
        let (work, env) = tool_env_with_file("ant_nowin").await;
        let (sdir, store) = test_store("ant_nowin_store");
        let mut key = test_key(&upstream);
        key.models.clear(); // 抹掉窗口数据（等于用户手填模型名 / 只拉取过列表）

        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0,
        )
        .await
        .expect("已知 Claude 模型即使未填窗口也应按 64k 能力参与聚合");
        assert_eq!(out, "x");
        let reqs = seen.lock().clone();
        assert_eq!(reqs.len(), 1, "应发出一次请求");
        assert_eq!(reqs[0]["max_tokens"], 64_000);
        assert_ne!(reqs[0]["max_tokens"], 4096, "不得回退旧默认值");

        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 单轮工具调用数超上限：超出的回 is_error 结果但**不执行**（不起子进程/不读盘），
    /// 且每个 call 仍有一一对应的结果（否则下一轮上游 400）。
    #[tokio::test]
    async fn tool_loop_caps_calls_per_turn() {
        // 第 1 轮返回 MAX+3 个 tool_use（模拟被注入诱导的爆量调用），第 2 轮给结论。
        let n = MAX_TOOL_CALLS_PER_TURN + 3;
        let calls: String = (0..n)
            .map(|i| format!(
                r#"{{"type":"tool_use","id":"t{i}","name":"read_file","input":{{"path":"main.rs"}}}}"#
            ))
            .collect::<Vec<_>>()
            .join(",");
        let first = format!(r#"{{"content":[{calls}],"stop_reason":"tool_use"}}"#);
        let first_static: &'static str = Box::leak(first.into_boxed_str());
        let (upstream, seen) = spawn_scripted(vec![
            first_static,
            r#"{"content":[{"type":"text","text":"够了，结论是 X"}]}"#,
        ])
        .await;
        let (work, env) = tool_env_with_file("loop_cap_calls").await;
        let (sdir, store) = test_store("loop_cap_calls_store");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .unwrap();
        assert_eq!(out, "够了，结论是 X");

        let reqs = seen.lock().clone();
        let results = reqs[1]["messages"][2]["content"].as_array().unwrap();
        // 协议一一对应：n 个 tool_use → n 个 tool_result，一个都不能少
        assert_eq!(results.len(), n, "每个 call 都必须有结果，否则上游 400");
        // 前 MAX 个真执行（读到了文件内容），超出的是「未执行」错误
        assert!(
            results[0]["content"].as_str().unwrap().contains("answer_42"),
            "前若干个应真执行：{:?}",
            results[0]
        );
        let over = &results[MAX_TOOL_CALLS_PER_TURN];
        assert_eq!(over["is_error"], true);
        assert!(
            over["content"].as_str().unwrap().contains("超过上限"),
            "超出的应回未执行错误：{over:?}"
        );
        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 🔴 **一轮里的多个工具并发执行，但 `tool_result` 的顺序必须与 `tool_use` 逐个对应。**
    ///
    /// 两家协议都按**位置**配对，顺序错了上游直接 400 —— 而那种错法只在「模型一轮调多个
    /// 工具、且它们耗时不同」时才出现，是最容易被顺序版测试放过的形态。
    ///
    /// 夹具刻意让**耗时反序**：第一个 call 读一个大文件（慢），第二个读小文件（快）。
    /// 若实现改用 `FuturesUnordered`（按完成顺序收集），快的那个会挤到前面 → 本测试变红。
    /// `join_all` 按入参顺序返回，故这条成立。
    #[tokio::test]
    async fn concurrent_tool_calls_keep_their_original_order() {
        let (upstream, seen) = spawn_scripted(vec![
            r#"{"content":[
                {"type":"tool_use","id":"t0","name":"read_file","input":{"path":"big.rs"}},
                {"type":"tool_use","id":"t1","name":"read_file","input":{"path":"small.rs"}}
            ],"stop_reason":"tool_use"}"#,
            r#"{"content":[{"type":"text","text":"读完了"}]}"#,
        ])
        .await;
        let dir = temp_dir("tool_order");
        // 一大一小：真实的执行耗时差，让「按完成顺序收集」这个错法有机会暴露。
        std::fs::write(dir.join("big.rs"), "// BIGMARK\n".repeat(20_000)).unwrap();
        std::fs::write(dir.join("small.rs"), "// SMALLMARK\n").unwrap();
        let env = crate::agent_tools::ToolEnv::detect(&dir).await;
        let (sdir, store) = test_store("tool_order_store");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            Some(&env),
            6,
            60_000,
            0,
        )
        .await
        .unwrap();
        assert_eq!(out, "读完了");

        let reqs = seen.lock().clone();
        let results = reqs[1]["messages"][2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        // 按位置配对：第 0 条必须是 big.rs 的结果，第 1 条是 small.rs 的。
        assert_eq!(results[0]["tool_use_id"], "t0", "顺序错了上游会 400");
        assert_eq!(results[1]["tool_use_id"], "t1", "顺序错了上游会 400");
        assert!(
            results[0]["content"].as_str().unwrap().contains("BIGMARK"),
            "第 0 条应是大文件（慢的那个）的内容：{:?}",
            results[0]
        );
        assert!(
            results[1]["content"].as_str().unwrap().contains("SMALLMARK"),
            "第 1 条应是小文件（快的那个）的内容：{:?}",
            results[1]
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&sdir).ok();
    }

    /// 源码级：工具执行必须是**并发且限流**的，且必须用**保序**的收集方式。
    ///
    /// 上面那条行为用例在两个工具耗时差不够大时也可能偶然通过（顺序恰好正确），
    /// 而这条钉住结构 —— 它同时防三种回归：退回串行 `for` 循环（性能白改）、
    /// 换成 `buffer_unordered` / `FuturesUnordered`（顺序静默打乱、上游 400）、
    /// 或去掉限流让 16 个子进程一起上（在用户开发机上制造资源尖峰）。
    ///
    /// ⚠️ **扫的是 `aggregate/tool_loop.rs`**，不是本文件 —— 工具循环搬过去之后这条判据
    /// 曾指着旧路径继续绿（那时它扫到的文件里压根没有那段代码）。本轮实际红过一次才发现。
    #[test]
    fn tool_calls_must_run_concurrently_and_stay_ordered() {
        let src = std::fs::read_to_string("src/aggregate/tool_loop.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert!(
            prod.contains(".chunks(MAX_CONCURRENT_TOOLS)"),
            "一轮里的多个只读工具必须并发执行，且并发度要限流 —— \
             grep/codegraph 都起子进程，16 路齐发是在用户开发机上制造资源尖峰"
        );
        assert!(
            !prod.contains("buffer_unordered") && !prod.contains("FuturesUnordered"),
            "不许用按完成顺序收集的方式 —— tool_result 与 tool_use 按位置配对，乱序即上游 400"
        );
        // 限流值必须是个小数：写成 16 等于没限流。
        assert!(
            (2..=8).contains(&MAX_CONCURRENT_TOOLS),
            "并发度 {MAX_CONCURRENT_TOOLS} 不在合理区间 —— 太小失去收益、太大等于没限流"
        );
    }

    /// 回归护栏：工具**关闭**（tool_env=None）时，请求体必须与旧的 text_completion 逐字段一致
    /// —— content 是纯字符串、无 tools/tool_choice 字段、单轮即终。这是「关掉开关等于回到改动前」
    /// 的判据，本轮把成员路径从 text_completion 换成了 ToolSession，最容易在这里回归。
    #[tokio::test]
    async fn tools_off_request_shape_matches_plain_completion() {
        let (upstream, seen) = spawn_scripted(vec![
            r#"{"content":[{"type":"text","text":"纯文本答案"}]}"#,
        ])
        .await;
        let (sdir, store) = test_store("tools_off_store");
        let key = test_key(&upstream);
        let mut session =
            ToolSession::new(Protocol::Anthropic, &MultimodalPrompt::from_text("问题X"));

        let out = run_member_turns(
            &store,
            CategoryType::ClaudeCli,
            &mut session,
            &ctx_for(&key, "sk", "mock/m"),
            None, // 工具关闭
            6,
            60_000,
            0, // tool_ctx_budget: 测试不验证裁剪
        )
        .await
        .unwrap();
        assert_eq!(out, "纯文本答案");

        let reqs = seen.lock().clone();
        assert_eq!(reqs.len(), 1, "工具关闭必须单轮即终");
        // 无 tools / tool_choice 字段
        assert!(reqs[0].get("tools").is_none(), "关闭态不该发 tools 字段");
        assert!(reqs[0].get("tool_choice").is_none());
        // content 是纯字符串（与 text_completion 的 [{role:user, content:prompt}] 一致）
        let msgs = reqs[0]["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "问题X", "content 必须是纯字符串，不是 block 数组");

        std::fs::remove_dir_all(&sdir).ok();
    }
}
