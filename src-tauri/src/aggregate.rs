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
use crate::model::{
    AggregateMode, AggregateResult, AppliedChange, BrainConfig, CategoryType, Protocol,
    RequestTrace,
};
use crate::retrieval;
use crate::store::Store;
use crate::upstream;
use crate::upstream::{ImagePart, MultimodalPrompt, ToolSession, TurnOutcome};
use futures_util::future::join_all;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// 聚合日志里请求/响应体的最大字符数（与调用模型日志同量级，防超大 prompt 撑爆内存日志）。
const AGG_LOG_CAP: usize = 20_000;

/// 截断超长文本，附省略提示（成员答案/汇总产物/决策者入参出参落日志用）。
fn cap_text(s: &str) -> String {
    if s.chars().count() <= AGG_LOG_CAP {
        s.to_string()
    } else {
        let head: String = s.chars().take(AGG_LOG_CAP).collect();
        format!("{head}\n…（已截断，共 {} 字符）", s.chars().count())
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

/// 决策者阶段的保底预算（毫秒）。
///
/// 整轮墙钟预算下，串行的「成员 → 压缩 → 决策者」三阶段共享同一 deadline。为避免前面
/// 阶段（成员慢/压缩慢）把时间吃光、饿死最重要的决策者综合步骤，给决策者留一块地板：
/// 整轮预算的 35%，绝对不低于 90s；但小预算时 90s 可能超过整轮总量，故再用 45% 上限夹住
/// （保证成员+压缩至少还能分到 ~55%）。
///
/// 例：total=60000 → 35%=21000<90000，被 45%=27000 夹住 → 27000；
///     total=257000 → 35%=89950<90000 → 90000（<45%=115650）；
///     total=540000 → 35%=189000（>90000，<45%=243000）→ 189000；
///     total=600000 → 35%=210000 → 210000。
fn decider_floor_ms(total_ms: u64) -> u64 {
    let pct35 = total_ms * 35 / 100;
    pct35.max(90_000).min(total_ms * 45 / 100)
}

/// 成员/压缩阶段的可用预算（毫秒）：在整轮 deadline 剩余时间里扣掉决策者地板。
///
/// `remaining_ms` 为此刻距整轮 deadline 的剩余毫秒。扣掉 `decider_floor` 后仍给一个最小
/// 下限（`min_floor`，默认 5s）——即使前面阶段几乎耗尽预算，也让本阶段有机会发出请求，
/// 略微越界由客户端超时余量兜底，好过给 0ms 必然超时。
fn upstream_phase_budget_ms(remaining_ms: u64, decider_floor: u64, min_floor: u64) -> u64 {
    remaining_ms.saturating_sub(decider_floor).max(min_floor)
}

/// 决策者阶段的可用预算（毫秒）：整轮剩余时间全给决策者（前面省下的它全拿），
/// 同样给最小下限保护，避免 0ms。
fn decider_phase_budget_ms(remaining_ms: u64, min_floor: u64) -> u64 {
    remaining_ms.max(min_floor)
}

/// 阶段预算的最小下限（毫秒）：宁可略微越过整轮 deadline，也要让阶段有机会跑一次
/// （客户端超时留有 +余量，见 tools.rs 的 mcp 客户端超时联动）。
const PHASE_MIN_BUDGET_MS: u64 = 5_000;

struct MemberAnswer {
    label: String,
    answer: String,
}

/// 成员一次实际调用的元信息（构造日志 trace 用）。
struct MemberCallMeta {
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

/// Phase1: 参与者思考 + 决策者输出修改计划。
pub async fn run_plan(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
) -> AppResult<AggregateResult> {
    let brain = store.get_brain(category);
    if !brain.enabled {
        return Err(AppError::Invalid("大脑聚合未启用".into()));
    }
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;

    // 整轮墙钟 deadline：串行的「成员 → 压缩 → 决策者」共享同一预算，各阶段按剩余时间递减
    // 分配，并给决策者留保底（见 decider_floor_ms）。总量始终压在客户端 MCP 超时之下，
    // 保证服务端能在客户端杀连接前优雅降级返回。
    let round_start = std::time::Instant::now();
    let deadline = round_start + Duration::from_millis(brain.total_timeout_ms);
    let decider_floor = decider_floor_ms(brain.total_timeout_ms);
    let remaining_ms = |d: std::time::Instant| {
        d.saturating_duration_since(std::time::Instant::now()).as_millis() as u64
    };

    // 0. 文件检索（work_dir 若开启自动跟随则取最新活跃项目）
    let effective_work_dir = resolve_work_dir(&brain);
    let file_context = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let max_tokens = brain.max_context_tokens;
            let outcome = retrieval::retrieve_detailed(work_dir, prompt, max_tokens).await;
            store.append_event(
                category,
                "aggregate",
                None,
                &format!("检索 · {} · 目录={work_dir}", outcome.summary),
            );
            for d in &outcome.diagnostics {
                store.append_event(category, "aggregate", None, &format!("检索诊断 · {d}"));
            }
            format_file_context(&outcome.files)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // 1. 构建参与者 prompt（只读角色）
    let member_prompt = build_member_prompt(prompt, &file_context);

    // 2. 并行成员解答（预算 = 剩余整轮时间 − 决策者保底）
    let members_budget = upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
    let tool_env = prepare_tool_env(store, category, &brain, effective_work_dir.as_deref()).await;
    // 桌面端 UI 这条路径不接受图片（只有 MCP 的 images 参数会传图，见 run_mcp）。
    let gathered =
        gather_members(store, category, &brain, &member_prompt, &[], tool_env, members_budget).await;
    let answers = &gathered.answers;

    if answers.is_empty() {
        let fallback_prompt = build_solo_decider_prompt(prompt, &file_context);
        // 独答降级：无成员/压缩阶段，决策者独享整轮剩余时间。
        // with_usage 包住：独答同样烧额度，桌面路径此前完全不记这笔账。
        let solo_budget = decider_phase_budget_ms(remaining_ms(deadline), PHASE_MIN_BUDGET_MS);
        let (fallback, solo_used) =
            upstream::with_usage(call_ref(store, &decider_ref, &fallback_prompt, solo_budget)).await;
        let fallback = fallback?;
        if !solo_used.is_empty() {
            store.append_event_full(
                category,
                "aggregate",
                None,
                &format!("独答降级 · {} · {}", label_ref(store, &decider_ref), solo_used.fmt_compact()),
                None,
                None,
                Some(solo_used),
            );
        }
        return Ok(AggregateResult::Plan {
            content: fallback,
            work_dir: effective_work_dir,
        });
    }

    // 3. 聚合
    let aggregated = match brain.aggregate_mode {
        AggregateMode::Full => build_full_context(answers),
        AggregateMode::Compressed => {
            let summarizer_ref = brain
                .summarizer_ref
                .clone()
                .unwrap_or_else(|| decider_ref.clone());
            // 压缩阶段：成员跑完后重算剩余，仍扣掉决策者保底。
            let compress_budget = upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
            match compress(store, category, &summarizer_ref, answers, compress_budget).await {
                Ok(s) => s,
                // 压缩失败（summarizer Key 限流/余额/被删）不作废已成功的成员回答，
                // 降级为全量拼接，避免整轮聚合白跑。
                Err(e) => {
                    tracing::warn!("compress 失败，降级为全量上下文: {e}");
                    build_full_context(answers)
                }
            }
        }
    };

    // 4. 决策者输出计划
    let plan_prompt = format!(
        "你是最终决策者。以下是多位代码审阅者的分析意见。\n\
         请综合所有意见，输出一份具体的修改计划：列出需要修改的文件路径和具体改动描述。\n\
         注意：现在只输出计划，不要输出代码。等用户确认后再执行。\n\n\
         ## 用户需求\n{prompt}\n\n\
         ## 审阅意见\n{aggregated}\n\n\
         {file_section}\
         请输出修改计划：",
        prompt = prompt,
        aggregated = aggregated,
        file_section = if file_context.is_empty() {
            String::new()
        } else {
            format!("## 相关文件\n{}\n\n", file_context)
        }
    );
    // 决策者阶段：整轮剩余时间全给它（前面阶段省下的它全拿，保底 decider_floor 已被保护）。
    let plan_budget = decider_phase_budget_ms(remaining_ms(deadline), PHASE_MIN_BUDGET_MS);
    // 决策者失败（限流/超时/余额/被删）**不作废**已成功的成员答案 —— 与 run_mcp 同口径
    // （那边注释明言「用户等了数十秒的多专家意见不能因决策者一句错就整轮丢失」）。
    // 降级：把已聚合的成员意见作为「计划」正文返回并在头部标注失败原因；此时它只是
    // 参考材料而非可执行计划，用户不该直接点「确认执行」——头部的警告就是说明这一点。
    // 本函数对压缩阶段失败（上方 compress 分支）早有同款降级，决策者阶段此前漏了。
    //
    // with_usage 包住：决策者是整轮最重的一笔（prompt 内嵌全部成员答案），
    // 桌面路径此前完全不记它的用量 —— 用户按日志对账必然偏低。失败也烧额度，同样要记。
    let (plan_res, decider_used) =
        upstream::with_usage(call_ref(store, &decider_ref, &plan_prompt, plan_budget)).await;
    let plan = match plan_res {
        Ok(p) => p,
        Err(e) => {
            store.append_event(
                category,
                "aggregate",
                None,
                &format!(
                    "决策者失败 · {} · {e} · 降级为「返回已聚合成员意见」",
                    label_ref(store, &decider_ref)
                ),
            );
            format!(
                "> ⚠️ 决策者 `{}` 未能完成综合分析：{e}\n\
                 > 以下是已成功获取的 {} 位专家意见（**不是可执行计划**，请勿直接确认执行）：\n\n{}",
                label_ref(store, &decider_ref),
                answers.len(),
                aggregated
            )
        }
    };
    // 整轮合计（成员 + 决策者）：与 run_mcp 同口径 —— 这是用户判断「一次会诊花了多少
    // 额度」的唯一入口，桌面路径此前没有这条，决策者的最大开销在账上完全消失。
    let mut grand = gathered.usage;
    grand.add(&decider_used);
    if !grand.is_empty() {
        store.append_event_full(
            category,
            "aggregate",
            None,
            &format!(
                "本次聚合合计 · {} · 共 {} tokens（成员 {} + 决策者 {}，不含压缩阶段）",
                grand.fmt_compact(),
                grand.total(),
                gathered.usage.total(),
                decider_used.total()
            ),
            None,
            None,
            // ⚠️ **不把 usage 交给累加器**：这只是一条「合计」展示行，分项已各自记过账
            // （成员在 gather_members、决策者在其调用点、压缩阶段自己记）。
            // append_event_full 对**任何**带 usage 的事件都无条件累加进 usage_totals，
            // 汇总行再带一次 = 用量面板与估算金额恒为真实值的 2 倍，且会落进 usage.json 日桶、
            // 重启后依旧翻倍、永不自愈。token 数已写在上面的 detail 文本里，展示不受影响。
            None,
        );
    }
    Ok(AggregateResult::Plan {
        content: plan,
        work_dir: effective_work_dir,
    })
}

/// Phase2: 用户确认计划后，决策者执行修改。
/// `pinned_work_dir` 为 Phase1 定下、前端回传的工作目录：优先使用它，避免 auto-follow
/// 期间用户切换活跃项目导致 apply 重新解析到另一个目录、把改动写错项目（目录漂移）。
pub async fn run_apply(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
    confirmed_plan: &str,
    pinned_work_dir: Option<String>,
) -> AppResult<AggregateResult> {
    let brain = store.get_brain(category);
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;

    // 优先用 Phase1 定下的目录；缺失时才回退实时解析（兼容老前端/直接调用）。
    // **关键防护**：若 Phase1 传了空字符串（而非 None），仍视为"已定下"，不再重新解析。
    // 否则前端传 `Some("")` 时会回退 resolve_work_dir，若用户在两个 phase 之间切换项目
    // 会导致 apply 写到错误的目录（目录漂移）。空字符串说明 Phase1 确认了「无工作目录」。
    let effective_work_dir = match &pinned_work_dir {
        Some(d) => {
            // Phase1 已定下（包括空字符串 = 确认无工作目录），不再重新解析
            if d.trim().is_empty() {
                None  // 明确无工作目录
            } else {
                Some(d.clone())
            }
        }
        None => {
            // Phase1 未传（老前端/直接调用）：回退实时解析
            resolve_work_dir(&brain)
        }
    };

    // 重新获取文件上下文
    let file_context = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let outcome =
                retrieval::retrieve_detailed(work_dir, prompt, brain.max_context_tokens).await;
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
            format!("## 当前文件内容\n{}\n\n", file_context)
        }
    );

    // with_usage 包住：Phase2 执行同样是决策者级别的大请求，账不能少这笔。
    let (result, exec_used) = upstream::with_usage(call_ref(
        store,
        &decider_ref,
        &exec_prompt,
        brain.total_timeout_ms,
    ))
    .await;
    let result = result?;
    if !exec_used.is_empty() {
        store.append_event_full(
            category,
            "aggregate",
            None,
            &format!("确认执行 · {} · {}", label_ref(store, &decider_ref), exec_used.fmt_compact()),
            None,
            None,
            Some(exec_used),
        );
    }

    // 解析输出中的 ```file:path\ncontent\n``` 块，写入工作目录
    let changes = if let Some(ref work_dir) = effective_work_dir {
        parse_and_apply(work_dir, &result)?
    } else {
        vec![]
    };

    // 对齐前端契约（content / filesModified）。成功写入的文件进 files_modified；
    // 失败的文件不静默——附到 content 末尾让用户在结果面板可见。
    let files_modified: Vec<String> =
        changes.iter().filter(|c| c.success).map(|c| c.path.clone()).collect();
    let failed: Vec<String> = changes
        .iter()
        .filter(|c| !c.success)
        .map(|c| format!("{}（{}）", c.path, c.error.as_deref().unwrap_or("写入失败")))
        .collect();
    let content = if failed.is_empty() {
        result
    } else {
        format!("{result}\n\n---\n⚠️ 以下文件写入失败：\n- {}", failed.join("\n- "))
    };

    Ok(AggregateResult::Applied { content, files_modified })
}

/// MCP 通道聚合结果（供 MCP Server 组装 Markdown 返回）。
pub struct McpAggregateResult {
    /// 决策者综合后的分析 / 修改计划（Markdown）
    pub analysis: String,
    /// 实际使用的工作目录（用于回显给客户端）
    pub work_dir: Option<String>,
    /// 参与分析的成员标签（keyName / modelName）
    pub member_labels: Vec<String>,
    /// 决策者引用（keyId::modelName）
    pub decider_ref: String,
    /// 注入的相关文件数
    pub file_count: usize,
    /// 被处理的成员数（含调用失败与前置不可用；不含被禁用而跳过的）
    pub members_attempted: usize,
    /// 调用失败 / 不可用的成员数
    pub members_failed: usize,
    /// 因 Key 被禁用而跳过的成员数
    pub members_skipped_disabled: usize,
}

/// MCP 通道专用聚合：只出建议 / 修改计划，绝不写文件（Q5）。
///
/// 与 run_plan 的差异：
/// - `cwd` 参数显式指定项目路径（来自 MCP 客户端），覆盖 brain 的 auto-follow / work_dir
/// - 返回结构化元信息（参与模型、文件数）供 MCP Server 组装 Markdown
/// - 每次调用独立成一次聚合任务，各项目并发互不影响（Q3）
pub async fn run_mcp(
    store: &Arc<Store>,
    category: CategoryType,
    prompt: &str,
    cwd: Option<String>,
    image_paths: Vec<String>,
) -> AppResult<McpAggregateResult> {
    let brain = store.get_brain(category);
    if !brain.enabled {
        return Err(AppError::Invalid(
            "该分类未启用大脑聚合，请先在 SynaRoute 桌面端配置参与者和决策者".into(),
        ));
    }
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;

    // 整轮墙钟 deadline（语义同 run_plan）：串行阶段共享预算，决策者留保底。
    let round_start = std::time::Instant::now();
    let deadline = round_start + Duration::from_millis(brain.total_timeout_ms);
    let decider_floor = decider_floor_ms(brain.total_timeout_ms);
    let remaining_ms = |d: std::time::Instant| {
        d.saturating_duration_since(std::time::Instant::now()).as_millis() as u64
    };

    // 工作目录优先级：MCP 显式 cwd > brain 配置（auto-follow / 手工 work_dir）
    let effective_work_dir = match cwd {
        Some(c) if !c.trim().is_empty() => Some(c),
        _ => resolve_work_dir(&brain),
    };

    // 图片：在这里加载而不是在 mcp.rs —— 图片路径相对于**同一个** effective_work_dir，
    // 两处各算一次工作目录必然漂移。任何校验不过都直接抛错，绝不静默丢图。
    let images = crate::agent_tools::load_images(
        effective_work_dir.as_deref(),
        &image_paths,
    )
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

    // 文件检索
    let files = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let outcome =
                retrieval::retrieve_detailed(work_dir, prompt, brain.max_context_tokens).await;
            // 检索路径与降级原因必须可见：旧实现里 codegraph 失败静默返回空，
            // 导致「集成从未生效」看起来像「没有命中」，排查无从下手。
            store.append_event(
                category,
                "aggregate",
                None,
                &format!("检索 · {} · 目录={work_dir}", outcome.summary),
            );
            for d in &outcome.diagnostics {
                store.append_event(category, "aggregate", None, &format!("检索诊断 · {d}"));
            }
            outcome.files
        } else {
            store.append_event(
                category,
                "aggregate",
                None,
                "检索已启用但无工作目录（未传 cwd、未开自动跟随、也未填手工目录），跳过",
            );
            vec![]
        }
    } else {
        vec![]
    };
    let file_count = files.len();
    let file_context = format_file_context(&files);

    // 成员清单（只展示配置里的参与者，固定调用，不做故障转移换 Key）
    let member_plan: Vec<String> = brain
        .members
        .iter()
        .map(|m| {
            let name = store
                .get_key(&m.key_id)
                .map(|k| k.name)
                .unwrap_or_else(|| m.key_id.clone());
            format!("{name}/{}" , m.model_name)
        })
        .collect();
    store.append_event(
        category,
        "aggregate",
        None,
        &format!(
            "大脑聚合开始 · 决策者={} · 汇总={} · 成员[{}] · 检索文件{} · 超时{}ms · 并发{}",
            label_ref(store, &decider_ref),
            brain
                .summarizer_ref
                .as_ref()
                .map(|r| label_ref(store, r))
                .unwrap_or_else(|| "复用决策者".into()),
            if member_plan.is_empty() {
                "无".into()
            } else {
                member_plan.join(" · ")
            },
            file_count,
            brain.total_timeout_ms,
            brain.concurrency_limit,
        ),
    );

    // 1. 参与者并行分析（只读）—— 每个成员固定打自己的 Key+模型，失败不换 Key
    //    预算 = 剩余整轮时间 − 决策者保底。
    let member_prompt = build_member_prompt(prompt, &file_context);
    let members_budget = upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
    let tool_env = prepare_tool_env(store, category, &brain, effective_work_dir.as_deref()).await;
    let gathered = gather_members(
        store,
        category,
        &brain,
        &member_prompt,
        &images,
        tool_env,
        members_budget,
    )
    .await;
    let answers = &gathered.answers;
    let member_labels: Vec<String> = answers.iter().map(|a| a.label.clone()).collect();
    let members_attempted = gathered.attempted;
    let members_failed = gathered.failed;
    let members_skipped_disabled = gathered.skipped_disabled;

    // 一条汇总落日志：N 发起 / M 成功 / F 失败 / D 禁用跳过，一眼看出是部分还是全部失败。
    store.append_event(
        category,
        "aggregate",
        None,
        &format!(
            "参与者汇总 · 发起 {} · 成功 {} · 失败 {} · 禁用跳过 {}{}",
            members_attempted,
            answers.len(),
            members_failed,
            members_skipped_disabled,
            if gathered.usage.is_empty() {
                String::new()
            } else {
                format!(" · 合计 {}", gathered.usage.fmt_compact())
            }
        ),
    );

    // 2. 无可用参与者 → 决策者独立回答（降级，不算失败）
    if answers.is_empty() {
        store.append_event(
            category,
            "aggregate",
            None,
            &format!(
                "无成功参与者，决策者独立分析 · {}",
                label_ref(store, &decider_ref)
            ),
        );
        let fallback_prompt = build_solo_decider_prompt(prompt, &file_context);
        // 独答降级：无成员/压缩阶段，决策者独享整轮剩余时间。
        let solo_budget = decider_phase_budget_ms(remaining_ms(deadline), PHASE_MIN_BUDGET_MS);
        let fallback = call_ref(store, &decider_ref, &fallback_prompt, solo_budget).await?;
        store.append_event(
            category,
            "aggregate",
            None,
            &format!("决策者返回 · {}", label_ref(store, &decider_ref)),
        );
        return Ok(McpAggregateResult {
            analysis: fallback,
            work_dir: effective_work_dir,
            member_labels,
            decider_ref,
            file_count,
            members_attempted,
            members_failed,
            members_skipped_disabled,
        });
    }

    // 3. 聚合
    let aggregated = match brain.aggregate_mode {
        AggregateMode::Full => {
            store.append_event(
                category,
                "aggregate",
                None,
                &format!("开始汇总 · 方式=全量上下文 · 成功成员 {}", answers.len()),
            );
            build_full_context(answers)
        }
        AggregateMode::Compressed => {
            let summarizer_ref = brain
                .summarizer_ref
                .clone()
                .unwrap_or_else(|| decider_ref.clone());
            store.append_event(
                category,
                "aggregate",
                None,
                &format!(
                    "开始汇总 · 方式=压缩 · 汇总模型={} · 成功成员 {}",
                    label_ref(store, &summarizer_ref),
                    answers.len()
                ),
            );
            // 压缩阶段：成员跑完后重算剩余，仍扣掉决策者保底。
            let compress_budget = upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
            match compress(store, category, &summarizer_ref, answers, compress_budget).await {
                Ok(s) => s,
                // 压缩失败（summarizer Key 限流/余额/被删）不作废已成功的成员回答，
                // 降级为全量拼接，避免整轮聚合白跑。
                Err(e) => {
                    tracing::warn!("compress 失败，降级为全量上下文: {e}");
                    store.append_event(
                        category,
                        "aggregate",
                        None,
                        &format!(
                            "汇总失败 · {} · {e} · 降级为全量上下文",
                            label_ref(store, &summarizer_ref)
                        ),
                    );
                    build_full_context(answers)
                }
            }
        }
    };

    // 4. 决策者综合 → 建议 / 修改计划
    //    强调「只出方案，不写代码」——由客户端（Codex/Claude Code）执行文件修改（Q5）。
    store.append_event(
        category,
        "aggregate",
        None,
        &format!(
            "交由决策者 · {} · 成功成员 {} · 失败成员 {}",
            label_ref(store, &decider_ref),
            answers.len(),
            members_failed
        ),
    );
    let decider_prompt = format!(
        "你是最终决策者。以下是多位专家顾问对同一个问题的独立分析意见。\n\
         请综合所有意见，给出一份清晰、可执行、直接回应用户问题的最终答案。\n\
         要求：\n\
         - 用 Markdown 组织，先给结论，再给依据/步骤/细节\n\
         - 根据问题类型自适应：代码/技术任务给出要改的文件路径与具体改动说明（涉及代码可给关键片段示例，但不要臆造完整文件）；\
         信息查询、方案设计、决策分析等问题则直接给出结论与理由，不要硬套「修改文件」的格式\n\
         - 标注顾问之间的分歧点（如有）\n\
         - 你只负责出答案/方案，实际的文件修改（如涉及）会由用户在客户端确认后执行\n\n\
         ## 用户问题\n{prompt}\n\n\
         ## 顾问意见\n{aggregated}\n\n\
         {file_section}\
         请输出你的综合答案：",
        prompt = prompt,
        aggregated = aggregated,
        file_section = if file_context.is_empty() {
            String::new()
        } else {
            format!("## 相关文件\n{}\n\n", file_context)
        }
    );
    let decider_started = std::time::Instant::now();
    // 决策者阶段：整轮剩余时间全给它（保底 decider_floor 已在前面阶段被保护）。
    let decider_budget = decider_phase_budget_ms(remaining_ms(deadline), PHASE_MIN_BUDGET_MS);
    let (decider_res, decider_used) = upstream::with_usage(call_ref(
        store,
        &decider_ref,
        &decider_prompt,
        decider_budget,
    ))
    .await;
    let analysis = match decider_res {
        Ok(a) => {
            let latency = decider_started.elapsed().as_millis() as u64;
            // 带 trace：展开可见「喂给决策者的完整入参（原问题+聚合意见+文件）+ 决策者最终答案」。
            store.append_event_full(
                category,
                "aggregate",
                None,
                &format!(
                    "决策者返回 · {} · {latency}ms{}",
                    label_ref(store, &decider_ref),
                    if decider_used.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", decider_used.fmt_compact())
                    }
                ),
                store.get_settings().aggregate_trace_enabled
                    .then(|| trace_for_ref(store, &decider_ref, &decider_prompt, &a, latency, true))
                    .flatten(),
                None,
                (!decider_used.is_empty()).then_some(decider_used),
            );
            a
        }
        // 决策者失败（限流/超时/余额/被删）不作废已成功的成员答案与聚合结果——
        // 用户等了数十秒的多专家意见不能因决策者一句错就整轮丢失。降级：把已聚合的
        // 成员答案作为最终返回，并在正文头部明确标注决策者失败原因，让用户看到中间产物。
        Err(e) => {
            let latency = decider_started.elapsed().as_millis() as u64;
            // 失败也带 trace：展开可见喂给决策者的完整入参 + 失败原因，便于排障。
            store.append_event_trace(
                category,
                "aggregate",
                None,
                &format!(
                    "决策者失败 · {} · {e} · 降级为「返回已聚合成员意见」",
                    label_ref(store, &decider_ref)
                ),
                store.get_settings().aggregate_trace_enabled
                    .then(|| trace_for_ref(store, &decider_ref, &decider_prompt, &e.to_string(), latency, false))
                    .flatten(),
            );
            format!(
                "> ⚠️ 决策者 `{}` 未能完成综合分析：{e}\n> 以下是已成功获取的 {} 位专家意见，供你参考：\n\n{}",
                label_ref(store, &decider_ref),
                answers.len(),
                aggregated
            )
        }
    };

    // 整次聚合的总账：这是用户判断「一次会诊花了多少额度」的唯一入口。
    //
    // 各环节的分项已各自落过日志，这里给合计 —— 没有它就只能靠人肉把
    // N 条成员日志加起来，而失败成员的消耗最容易被漏掉。
    let mut grand = gathered.usage;
    grand.add(&decider_used);
    if !grand.is_empty() {
        store.append_event_full(
            category,
            "aggregate",
            None,
            &format!(
                "本次聚合合计 · {} · 共 {} tokens（成员 {} + 决策者 {}，不含压缩阶段）",
                grand.fmt_compact(),
                grand.total(),
                gathered.usage.total(),
                decider_used.total()
            ),
            None,
            None,
            // ⚠️ **不把 usage 交给累加器**：这只是一条「合计」展示行，分项已各自记过账
            // （成员在 gather_members、决策者在其调用点、压缩阶段自己记）。
            // append_event_full 对**任何**带 usage 的事件都无条件累加进 usage_totals，
            // 汇总行再带一次 = 用量面板与估算金额恒为真实值的 2 倍，且会落进 usage.json 日桶、
            // 重启后依旧翻倍、永不自愈。token 数已写在上面的 detail 文本里，展示不受影响。
            None,
        );
    }

    Ok(McpAggregateResult {
        analysis,
        work_dir: effective_work_dir,
        member_labels,
        decider_ref,
        file_count,
        members_attempted,
        members_failed,
        members_skipped_disabled,
    })
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

fn build_member_prompt(prompt: &str, file_context: &str) -> String {
    let file_section = if file_context.is_empty() {
        String::new()
    } else {
        format!("\n\n## 相关文件（只读）\n{}", file_context)
    };
    format!(
        "你是一位专家顾问，正在与其他专家并行会诊同一个问题。请针对下面的问题给出你独立、专业的分析和见解。\n\
         - 若是技术/代码问题：指出关键点、风险与改进建议，涉及文件时说明是哪些文件、为什么（提供了文件时结合文件内容作答）。\n\
         - 若是信息查询、方案设计、技术选型、决策分析或其他问题：直接给出你的分析、依据和结论。\n\
         - 你只负责分析与建议，不执行任何修改或写入。\n\n\
         ## 问题\n{prompt}{file_section}"
    )
}

/// 无任何成功参与者时，决策者「独立作答」用的 prompt。
///
/// 必须把已检索到的 `file_context` 一并带上——否则检索已花的开销白费，且决策者在缺文件
/// 上下文下盲答，质量明显下降。正常聚合路径（decider_prompt / plan_prompt）都会拼
/// `## 相关文件`，降级路径若只发原始 prompt 就丢了这段上下文，此处补齐保持一致。
fn build_solo_decider_prompt(prompt: &str, file_context: &str) -> String {
    if file_context.is_empty() {
        return prompt.to_string();
    }
    format!(
        "{prompt}\n\n## 相关文件\n{file_context}\n\n\
         请结合以上相关文件，给出清晰、可执行、直接回应用户问题的答案。"
    )
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

/// 工具循环里探索阶段可用的预算占比；剩下的留给「强制出结论」那一轮。
///
/// 不留这一份，最后一次调用会在刚发出时被外层 timeout 掐掉 —— 前面几轮挖到的信息全白费，
/// 用户看到的只是「超时」。
const TOOL_LOOP_EXPLORE_PCT: u64 = 70;

/// 工具轮数的运行时上下限。上限防配置写了个 999 把预算烧光；下限保证「开了工具至少能真调一次」
/// （只给 1 轮时收尾逻辑会立刻生效，等于开了开关却没有工具）。
const TOOL_ROUNDS_RANGE: (u32, u32) = (2, 12);

/// 工具历史字符预算的 clamp 区间。
/// 下限 8000 = 一次工具结果的上限（RESULT_CHAR_CAP），再小就等于连一条完整结果都留不住；
/// 上限 400000（约 20 万 token）够极端场景，再大就失去「控膨胀」的意义。
const TOOL_CTX_BUDGET_RANGE: (usize, usize) = (8_000, 400_000);

/// **单轮**内实际执行的工具调用数上限。超出的回一条 is_error 结果但不执行（不起子进程、不读盘）。
///
/// 为什么需要：模型一轮可以返回任意多个 tool_use（协议无上界）。一份被提示注入的文件能诱导它
/// 一轮返回上百个 → 上百次串行子进程 + 上百条 8000 字符结果塞进历史 → 下一轮重发完整历史 →
/// token 爆炸。轮数上限只管轮数、不管单轮宽度，这条补上宽度。16 对正常"并行读几个文件"够用。
const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// 本次成员调用实际跑几轮。
///
/// 无工具时恒为 1（形状与旧的单轮补全完全一致）；有工具时把配置值夹进
/// [`TOOL_ROUNDS_RANGE`] —— 夹下限是关键：配 1 轮会让第一轮就走收尾分支，
/// 工具声明了却永远调不动，正是「开关开着但功能没生效」那类静默失效。
fn effective_rounds(has_tools: bool, configured: u32) -> u32 {
    if has_tools {
        configured.clamp(TOOL_ROUNDS_RANGE.0, TOOL_ROUNDS_RANGE.1)
    } else {
        1
    }
}

/// 成员的一次调用：工具关闭时是单轮普通补全，开启时是受限的 agent 循环。
///
/// 四条护栏，缺任何一条都有真实故障：
/// 1. **轮数上限**：模型会陷入「读文件 → 再读 → 再读」，不设上限能烧掉整轮预算。
/// 2. **时间预算**：探索只用前 [`TOOL_LOOP_EXPLORE_PCT`]%，余量留给收尾轮。
/// 3. **工具失败不中断**：错误文本作为 `tool_result` 回给模型让它换方向 —— 一个不存在的
///    文件名不该毁掉整次聚合。
/// 4. **收尾轮仍声明 tools**：Anthropic 规定「消息里出现过 tool_use/tool_result 就必须带 tools」，
///    收尾时抽掉 tools 会直接 400。故改用一条 user 指示让它别再调；它若仍要调，就采用
///    已积累的正文交差，而不是白跑一轮。
// 参数确实多，但每个都是这条链路必需的运行时上下文（store/分类用于写日志、session 是可变状态、
// 其余三个是配额与预算）。抽成 struct 只是把同样的东西换个地方传，还要动 7 个调用点 ——
// 与 `Store::append_event_full` 同样的取舍。
#[allow(clippy::too_many_arguments)]
async fn run_member_turns(
    store: &Arc<Store>,
    category: CategoryType,
    session: &mut ToolSession,
    ctx: &MemberCallCtx<'_>,
    tool_env: Option<&crate::agent_tools::ToolEnv>,
    max_rounds: u32,
    budget_ms: u64,
    // 工具结果历史的字符预算：累计超过就把较早轮次的结果压成占位（见 trim_tool_history）。
    // 传 0 = 不裁剪（关闭该保护）。
    tool_ctx_budget: usize,
) -> Result<String, MemberError> {
    let tools = match tool_env {
        Some(env) => crate::agent_tools::tool_defs(env),
        None => Vec::new(),
    };
    // 无工具 → 一轮即终；请求形状与旧的 text_completion 完全一致（content 是字符串、无 tools 字段）。
    let rounds = effective_rounds(!tools.is_empty(), max_rounds);
    let started = std::time::Instant::now();
    let explore_deadline = started + Duration::from_millis(budget_ms * TOOL_LOOP_EXPLORE_PCT / 100);
    // 整个成员循环的硬截止。外层 `gather_members` 也套了 `timeout(budget_ms)`，但**外层先到
    // 等于全盘皆输**：future 在 HTTP await 上被丢弃，前几轮挖到的 `preamble` 连同已花掉的
    // token 一起作废，用户只看到「超时」。故这里自己先到点，把已有正文交出去。
    let hard_deadline = started + Duration::from_millis(budget_ms);
    // 留给「内层先于外层返回」的余量：单轮超时按此往回缩，收到 reqwest 超时错误后还有时间
    // 走完下面的日志与返回，不至于在 return 的路上被外层掐掉。
    const TURN_MARGIN: Duration = Duration::from_millis(750);

    // 模型在调工具前说的话。轮数/时间到顶时用它交差，比返回空串有用。
    let mut preamble = String::new();
    for round in 1..=rounds {
        let now = std::time::Instant::now();
        let out_of_time = round > 1 && now >= explore_deadline;
        let wrapping_up = round == rounds || out_of_time;
        if wrapping_up && round > 1 {
            session.push_user_note(
                "已达到工具调用上限，请不要再调用工具，直接基于目前已获得的信息给出完整分析与结论。",
            );
        }
        // 单轮超时 = min(Key 级超时, 距硬截止的剩余量 - 余量)。
        // 不夹这一刀，`request_timeout`（budget+5s）永远比外层 timeout 长，上面那段注释里的
        // 「外层先到」就是必然结果而非边缘情况。
        let left = hard_deadline.saturating_duration_since(now);
        let turn_budget = left.saturating_sub(TURN_MARGIN);
        if turn_budget.is_zero() {
            // 连发一次请求的时间都没有了：有正文就交差，没有就如实报超时。
            return if preamble.trim().is_empty() {
                Err(MemberError::own(format!("超时（>{budget_ms}ms）且未给出结论")))
            } else {
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!(
                        "工具循环收尾 · {} · 第 {round}/{rounds} 轮前时间预算已用尽，采用已有正文",
                        ctx.label
                    ),
                );
                Ok(preamble)
            };
        }
        let mut up = ctx.up;
        up.request_timeout = up.request_timeout.min(turn_budget);
        // 输出预算**每轮重算**：工具循环每轮重发整份历史，历史越长可用输出空间越小。
        // 首轮算一次然后沿用会在后面几轮把 max_tokens 算得过大 → 输入+输出超窗口 → 400。
        // OpenAI 侧恒为 None（不发上限）；Anthropic 侧按「窗口 − 本轮输入」现算。
        //
        // `Err` 有两种：① 缺能力数据（确实只会第 1 轮命中，同一 Key 判定不随轮次变化）；
        // ② **输入占满窗口**（`input_exhausts_context_reason`）—— 它取决于逐轮膨胀的
        // 历史（工具返回的大段文件内容每轮重发），完全可能在第 2~N 轮才越线。
        // 后者发生时前几轮已产出的 preamble 仍然值钱，必须与下方 turn 失败分支同口径
        // 「有正文就交差」，而不是把整个成员判失败、已读到的信息与已烧的 token 全作废。
        up.max_tokens = match upstream::output_budget(
            up.key,
            up.model,
            session.estimated_input_tokens(&tools),
        ) {
            Ok(v) => v,
            Err(e) if !preamble.trim().is_empty() => {
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!(
                        "工具循环中断 · {} · 第 {round}/{rounds} 轮输入已占满上下文窗口（{e}），采用已有正文",
                        ctx.label
                    ),
                );
                return Ok(preamble);
            }
            Err(e) => return Err(MemberError::own(e)),
        };
        let outcome = match session.turn(&up, &tools).await {
            Ok(o) => o,
            // 轮内失败：已有正文就交差而不是整次作废（多轮探索的价值就在这里 —— 第 3 轮
            // 超时/报错，前 2 轮读到的东西仍然值钱）。失败原因写进日志，不静默。
            Err(e) if !preamble.trim().is_empty() => {
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!(
                        "工具循环中断 · {} · 第 {round}/{rounds} 轮调用失败（{e}），采用已有正文",
                        ctx.label
                    ),
                );
                return Ok(preamble);
            }
            // 唯一携带上游状态码的失败路径：交给 MemberError 结构化保存，供 4xx 判定使用。
            Err(e) => return Err(MemberError::from_upstream(&e)),
        };
        match outcome {
            TurnOutcome::Text(t) if !t.trim().is_empty() => return Ok(t),
            TurnOutcome::Text(_) => {
                // 空文本：有铺垫就用铺垫，否则如实返回空（上层判为「返回空答案」）。
                return Ok(preamble);
            }
            TurnOutcome::ToolCalls { text, calls, .. } => {
                if !text.trim().is_empty() {
                    if !preamble.is_empty() {
                        preamble.push_str("\n\n");
                    }
                    preamble.push_str(&text);
                }
                if wrapping_up {
                    store.append_event(
                        category,
                        "aggregate",
                        None,
                        &format!(
                            "工具循环收尾 · {} · 第 {round}/{rounds} 轮仍请求调用工具{}，采用已有正文",
                            ctx.label,
                            if out_of_time { "（时间预算已用尽）" } else { "" }
                        ),
                    );
                    return if preamble.trim().is_empty() {
                        Err(MemberError::own(format!(
                            "工具调用已达 {rounds} 轮上限且未给出结论"
                        )))
                    } else {
                        Ok(preamble)
                    };
                }
                let Some(env) = tool_env else {
                    // 本次没声明任何工具却回了 tool_use：上游协议异常，如实报出不静默。
                    return Err(MemberError::own(
                        "上游返回了工具调用，但本次未声明任何工具",
                    ));
                };
                // 单轮工具调用数上限。**每个 call 仍必须回一条结果**（两家协议都要求一一对应，
                // 缺一条上游直接 400），故超限的不是丢弃、而是回一条 is_error 结果且**不执行**
                // （不起子进程、不读盘、不占 8000 字符预算）。这样既堵住「一份被注入的文件诱导
                // 模型一轮返回上百个 tool_use → 上百次串行子进程 + 历史爆炸 → 下轮重发天价 token」，
                // 又保持协议正确。
                if calls.len() > MAX_TOOL_CALLS_PER_TURN {
                    store.append_event(
                        category,
                        "aggregate",
                        None,
                        &format!(
                            "工具调用过多 · {} · 单轮 {} 个，仅执行前 {}，其余回退让模型减少调用",
                            ctx.label,
                            calls.len(),
                            MAX_TOOL_CALLS_PER_TURN
                        ),
                    );
                }
                let mut results = Vec::with_capacity(calls.len());
                for (idx, c) in calls.iter().enumerate() {
                    if idx >= MAX_TOOL_CALLS_PER_TURN {
                        // 超限：回一条错误结果，不执行。模型据此在下一轮减少调用数。
                        results.push(crate::agent_tools::over_limit_result(
                            c,
                            MAX_TOOL_CALLS_PER_TURN,
                        ));
                        continue;
                    }
                    let r = crate::agent_tools::execute(env, c).await;
                    // 折叠只对**成功**的同名连续调用生效（降噪）。**失败/被拒不折叠**：
                    // collapse_key=None 让每条被拒都在 UI 留独立一行 —— 折叠会用最后一条
                    // 覆盖 detail，把「模型连读了两个 .env 被拒、第三个 main.rs 成功」抹成
                    // 一条「read_file main.rs ×3」，正好埋掉安全审计最需要看到的信号
                    // （提示注入诱导模型连试凭据文件时就是这个相邻同工具的形态）。
                    let collapse_key = if r.is_error {
                        None
                    } else {
                        Some(format!("tool:{}:{}", ctx.label, c.name))
                    };
                    store.append_event_collapsible(
                        category,
                        "aggregate",
                        None,
                        &format!(
                            "工具 · {} · {} {}{}",
                            ctx.label,
                            c.name,
                            tool_call_brief(c),
                            if r.is_error { " · 被拒/失败" } else { "" }
                        ),
                        None,
                        collapse_key,
                    );
                    results.push(r);
                }
                // 每个 call 都必须有结果：两家协议都要求一一对应，缺一条上游直接 400。
                session.push_tool_results(&results);
                // 控制历史膨胀：工具循环**每轮都重发完整历史**，真机实测一次成员调用的
                // 请求体峰值达 20 万字符（约 10 万 token）。超预算时把较早轮次的工具结果
                // 正文压成占位（消息条数与 id 配对不动，见 trim_tool_history）。
                // 预算 0 = 该保护关闭，完全不裁剪。
                let squashed = if tool_ctx_budget > 0 {
                    session.trim_tool_history(tool_ctx_budget)
                } else {
                    0
                };
                if squashed > 0 {
                    store.append_event(
                        category,
                        "aggregate",
                        None,
                        &format!(
                            "上下文裁剪 · {} · 已省略较早的 {squashed} 条工具结果（预算 {tool_ctx_budget} 字符）",
                            ctx.label
                        ),
                    );
                }
            }
        }
    }
    // rounds >= 1 且每个分支都 return，正常到不了这里。
    Ok(preamble)
}

/// 工具调用的一行摘要（进日志）。参数值可能是整段正则/长路径，统一截到 80 字符。
fn tool_call_brief(c: &upstream::ToolInvocation) -> String {
    let Some(obj) = c.args.as_object() else {
        return "[参数非法]".into();
    };
    let mut parts: Vec<String> = obj
        .iter()
        .map(|(k, v)| {
            let s = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string());
            format!("{k}={s}")
        })
        .collect();
    parts.sort(); // 顺序稳定，折叠 key 才不会因参数顺序抖动而失效
    let s = parts.join(" ");
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
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
    let sem = Arc::new(tokio::sync::Semaphore::new(
        brain.concurrency_limit.max(1) as usize,
    ));
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
    let brain_concurrency = brain.concurrency_limit.max(1);

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
            // 禁用的 Key 不参与聚合（此前遗漏此判断，导致禁用 Key 仍被调用）。
            if !key.enabled {
                return MemberOutcome::SkippedDisabled { label };
            }
            // 大脑聚合：成员固定 Key，不做故障转移换 Key。
            // 仅在「明确熔断窗口内」跳过该成员（真实流量连续失败触发），探测 Down 不挡路由。
            // 熔断中跳过 = 避免白等超时，不是故障转移。
            if let Some(until) = key.health.breaker_until {
                if until > chrono::Utc::now().timestamp_millis() {
                    return MemberOutcome::Unavailable {
                        label,
                        reason: "熔断窗口内，本轮跳过（聚合不做故障转移换 Key）".into(),
                    };
                }
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
                    None,
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
                let failed_usage = meta.as_ref().map(|m| m.usage).unwrap_or_default();
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
                    None,
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
                    &format!("参与者已禁用，跳过 · {label}（在分类页重新启用后才会参与聚合）"),
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
        upstream::with_usage(call_ref(store, summarizer_ref, &sum_prompt, budget_ms)).await;
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
        None,
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

fn format_file_context(files: &[retrieval::RetrievedFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for f in files {
        s.push_str(&format!(
            "\n### {} ({})\n```\n{}\n```\n",
            f.path, f.source, f.content
        ));
    }
    s
}

/// 判断 LLM 给出的相对路径是否安全（禁止逃逸 work_dir）。
/// 拒绝：绝对路径、盘符/UNC 前缀、根、任何 `..` 组件。
///
/// `pub(crate)`：`agent_tools` 的只读工具收到的路径同样来自模型输出，同样不可信，
/// 走同一道字符串级校验（两处各写一份必然漂移）。
pub(crate) fn is_safe_relative_path(path: &str) -> bool {
    use std::path::Component;

    if path.trim().is_empty() {
        return false;
    }

    // ---- 与平台无关的字符串级判定（必须在 `Path` 语义之前）----
    //
    // 为什么不能只靠 `Path::is_absolute()` / `Component::Prefix`：**它们是平台相关的**。
    // 同一个字符串在两个平台上被判成两回事（macOS CI 首跑实测）：
    //
    // | 输入 | Windows | Unix |
    // |---|---|---|
    // | `C:\Windows\win.ini` | 绝对路径 + Prefix → 拒 | 一个普通组件 → **放行** |
    // | `C:/Windows/x` | 绝对路径 + Prefix → 拒 | 组件 `C:`/`Windows`/`x` → **放行** |
    // | `\\server\share\x` | UNC Prefix → 拒 | 一个普通组件 → **放行** |
    //
    // 在 Unix 上这三种不构成逃逸（反斜杠是合法文件名字符，落点仍在工作目录内，
    // 且第 2 道 canonicalize 判定照样成立）。但本函数的**承诺**是「拒 `..`、绝对路径、
    // 盘符、UNC」（见 `agent_tools` 模块注释），承诺在某个平台上不成立就是缺陷——
    // 下一个人会按注释信任它。且同一份模型输出在两平台行为分叉，无谓地增加排障面。
    //
    // 故这三类一律按**字符串形状**拒掉，与运行平台无关。

    // 盘符前缀：`X:` 开头（`C:\x`、`c:/x`、甚至裸 `C:`）
    let b = path.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return false;
    }
    // 反斜杠开头：UNC（`\\server\share`）与盘内根（`\Windows`）
    if path.starts_with('\\') {
        return false;
    }
    // 反斜杠也当分隔符做 `..` 判定 —— Unix 的 `Path` 不这么看，
    // 于是 `a\..\..\b` 在 Unix 上是一个组件、逃不掉但也拒不掉。
    if path
        .split(['/', '\\'])
        .any(|seg| seg == "..")
    {
        return false;
    }

    // ---- 平台原生判定（Windows 上仍是主力，Unix 上负责 `/` 开头与 `..` 组件）----
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return false;
    }
    for c in p.components() {
        match c {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return false,
            _ => {}
        }
    }
    true
}

/// 规范化后判断 `candidate` 是否仍在 `work_root` 之下（解析符号链接后的真实落点）。
///
/// 任一侧 canonicalize 失败（权限/竞态）时**判为不安全**——宁可拒写一次让用户重试，
/// 也不在无法确认落点时冒险写盘。
///
/// `pub(crate)`：`agent_tools` 的只读工具用它做「解析链接后仍在工作目录内」这道判定。
/// 读路径不需要 [`check_no_link_escape`]（那道为「目标尚不存在的写入」设计），因为读的目标
/// 必然已存在，直接 canonicalize 目标本身即可同时暴露「链接目录」与「目标自身是链接」两种逃逸。
pub(crate) fn is_within_work_root(
    work_root: &std::path::Path,
    candidate: &std::path::Path,
) -> bool {
    match (work_root.canonicalize(), candidate.canonicalize()) {
        (Ok(root), Ok(c)) => c.starts_with(root),
        _ => false,
    }
}

/// 落盘前的链接逃逸检查。返回 `Err(原因)` 即拒绝写入。
///
/// 两条独立的逃逸路径，必须都堵：
///
/// 1. **穿透链接目录建新子目录**。只校验「父目录存在时的父目录」是不够的：`vendor/` 是指向
///    外部的目录链接时，`vendor/sub/x.txt` 的父目录 `vendor/sub` **不存在**，校验被跳过，
///    紧随其后的 `create_dir_all` 会沿链接把目录建到外面去。故这里向上找到**最近一个已存在的
///    祖先**再校验——链接必然在这个祖先或它之下的某一段里，canonicalize 它就能暴露真实落点。
///    （Windows 上 junction 普通权限即可创建，pnpm 建 `node_modules/*` 就在用，不是刻意构造。）
///
/// 2. **目标文件自身是符号链接**。`fs::write` 跟随链接写入其目标：仓库里带一个
///    `notes.md` → `~/.ssh/config` 的文件链接（git 能 checkout 链接），父目录校验完全通过，
///    却把链接目标整份覆盖。故对已存在的目标用 `symlink_metadata`（不跟随链接）判类型，
///    是链接就拒。
///
/// 判据都取自文件系统而非 LLM 给的字符串——`is_safe_relative_path` 那道只看字符串，
/// 看不见链接。
fn check_no_link_escape(
    work_root: &std::path::Path,
    full_path: &std::path::Path,
) -> Result<(), String> {
    // ① 最近的已存在祖先必须在 root 之内。
    let mut probe = full_path.parent();
    while let Some(dir) = probe {
        if dir.exists() {
            if !is_within_work_root(work_root, dir) {
                return Err(
                    "目标解析后落在工作目录之外（疑似链接目录逃逸），已拒绝写入".into()
                );
            }
            break;
        }
        probe = dir.parent();
    }
    // 一路向上都不存在（work_dir 本身都没了）：无法确认落点，fail-closed。
    if probe.is_none() {
        return Err("工作目录不存在或无法解析，已拒绝写入".into());
    }

    // ② 目标本身不得是符号链接（用 symlink_metadata，它不跟随链接）。
    if let Ok(md) = std::fs::symlink_metadata(full_path) {
        if md.file_type().is_symlink() {
            return Err("目标是符号链接，写入会覆盖链接指向的文件，已拒绝写入".into());
        }
    }
    Ok(())
}

/// 解析决策者输出中的 ```file:path 代码块并写入磁盘。
///
/// 安全与健壮性：
/// - 路径遏制（两道）：① 组件检查拒绝 `../`、绝对路径、盘符/UNC；② 落盘前把父目录
///   canonicalize 后确认仍在 work_dir 之下，堵住符号链接逃逸（组件检查看字符串，看不见链接）。
///   两道都为了防提示注入——prompt 里混着检索到的项目文件内容，LLM 输出的路径不可信。
/// - 围栏解析：用「起始围栏的反引号数量」匹配对应长度的闭合围栏（≥3 个反引号且仅由反引号构成），
///   使文件内容里出现的 ``` 三反引号不会提前截断；找不到闭合围栏则丢弃该残块（不写截断文件）。
fn parse_and_apply(work_dir: &str, output: &str) -> AppResult<Vec<AppliedChange>> {
    let mut changes = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let work_root = std::path::Path::new(work_dir);

    let mut i = 0;
    while i < lines.len() {
        // 起始围栏：以 N 个反引号（N≥3）紧跟 "file:" 开头
        let fence_len = lines[i].chars().take_while(|&c| c == '`').count();
        let after_fence = &lines[i][fence_len..];
        if fence_len >= 3 && after_fence.starts_with("file:") {
            let path = after_fence["file:".len()..].trim().to_string();
            i += 1;
            let start = i;
            // 闭合围栏：整行仅由 ≥fence_len 个反引号构成
            let mut closed = false;
            while i < lines.len() {
                let l = lines[i].trim_end();
                if l.len() >= fence_len && l.chars().all(|c| c == '`') {
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                // 未找到闭合围栏：视为不完整输出，丢弃该块（不写截断文件），停止扫描
                changes.push(AppliedChange {
                    path,
                    success: false,
                    error: Some("代码块未正确闭合，已跳过（避免写入截断文件）".into()),
                });
                break;
            }
            let content = lines[start..i].join("\n");
            i += 1; // 跳过闭合围栏

            // 路径遏制：拒绝越界路径
            if !is_safe_relative_path(&path) {
                changes.push(AppliedChange {
                    path,
                    success: false,
                    error: Some("路径越界（含 .. / 绝对路径 / 盘符），已拒绝写入".into()),
                });
                continue;
            }

            // 二次防线：从文件系统层面确认真实落点仍在 work_dir 之下。
            //
            // 为什么不能只靠 `is_safe_relative_path` 的组件检查：它看的是 LLM 给的**字符串**，
            // 而符号链接是文件系统层面的事。若 work_dir 下存在一个指向外部的链接目录
            // （`link/` → `C:\Windows`），`link/x.dll` 这个路径不含 `..`、不是绝对路径，
            // 组件检查完全放行，但实际写到了工作目录之外。
            //
            // 两条逃逸路径（穿透链接目录建新子目录 / 目标本身是链接）都在
            // `check_no_link_escape` 里堵，详见该函数注释。
            let full_path = work_root.join(&path);
            if let Err(reason) = check_no_link_escape(work_root, &full_path) {
                changes.push(AppliedChange { path, success: false, error: Some(reason) });
                continue;
            }

            if let Some(parent) = full_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&full_path, &content) {
                Ok(_) => changes.push(AppliedChange { path, success: true, error: None }),
                Err(e) => changes.push(AppliedChange {
                    path,
                    success: false,
                    error: Some(e.to_string()),
                }),
            }
        } else {
            i += 1;
        }
    }
    Ok(changes)
}

/// 调用某个 `keyId::model` 引用做一次文本补全（决策者 / 汇总者 / 降级独答）。
///
/// `budget_ms` 为**单次调用**墙钟预算（取 brain.total_timeout_ms，与成员调用同一口径）：
/// 成员阶段早已按此预算逐个 timeout，但决策者 / 汇总阶段此前**无任何聚合级超时**，只受
/// reqwest 单 Key 超时 × 重试约束，导致聚合总墙钟不受控、可能超过 MCP 客户端首字节预算而
/// 被客户端断开、整轮白跑。此处对每一阶段都套同一预算，让「总超时」名副其实、逐阶段封顶。
async fn call_ref(
    store: &Arc<Store>,
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
    // 禁用的 Key 不发任何请求：用户禁用常因欠费/出问题（想止损），而决策者请求
    // 内嵌全部成员答案、是整轮最重的一笔。成员路径早已跳过禁用 Key（gather_members
    // 注释自认「此前遗漏此判断」是缺陷），这里必须同口径 —— 否则用户在 Key 页禁用
    // 决策者后聚合照常烧它的额度，界面日志无任何提示，正是最忌讳的静默失效。
    // 错误文案点明去哪修（换决策者/汇总者，或重新启用该 Key）。
    if !key.enabled {
        return Err(AppError::Invalid(format!(
            "Key「{}」已被禁用，无法作为决策者/汇总者调用。\
             请到「大脑聚合」页换一条启用中的 Key，或到分类页重新启用它。",
            key.name
        )));
    }
    let secret = store
        .secrets
        .read()
        .get(key_id)?
        .ok_or_else(|| AppError::Invalid("决策者密钥缺失".into()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn safe_relative_path_rejects_escapes() {
        // 允许：普通相对路径（含子目录）。
        for p in ["a.rs", "src/main.rs", "src/deep/nested/mod.rs", "./a.rs"] {
            assert!(is_safe_relative_path(p), "{p:?} 应被允许");
        }
        // 拒绝：父目录逃逸、绝对路径、盘符、UNC、根。
        for p in [
            "../secret.txt",
            "src/../../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "C:\\Windows\\System32\\drivers\\etc\\hosts",
            "C:/Windows/x",
            "\\\\server\\share\\x",
            "",
            "   ",
        ] {
            assert!(!is_safe_relative_path(p), "{p:?} 必须被拒绝（可写到工作目录之外）");
        }
    }

    #[test]
    fn parse_and_apply_writes_files_and_creates_parent_dirs() {
        let dir = temp_dir("apply_ok");
        let work = dir.to_string_lossy().to_string();
        let output = "决策者说明文字（不应被当成文件）\n\
             ```file:src/a.rs\n\
             fn a() {}\n\
             ```\n\
             中间穿插的散文\n\
             ```file:deep/nested/b.txt\n\
             line1\n\
             line2\n\
             ```\n";

        let changes = parse_and_apply(&work, output).unwrap();
        assert_eq!(changes.len(), 2, "应解析出两个文件块: {changes:?}");
        assert!(changes.iter().all(|c| c.success), "均应写入成功: {changes:?}");

        assert_eq!(
            std::fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "fn a() {}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("deep/nested/b.txt")).unwrap(),
            "line1\nline2",
            "多级父目录应被自动创建"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_and_apply_refuses_paths_escaping_work_dir() {
        // 路径遏制的端到端验证：越界块必须**既不写盘、也不静默**（要报回 success:false）。
        let dir = temp_dir("apply_escape");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("pwned.txt");

        let output = format!(
            "```file:../pwned.txt\nHACKED\n```\n\
             ```file:{}\nHACKED\n```\n\
             ```file:ok.rs\nfine\n```\n",
            outside.display()
        );
        let changes = parse_and_apply(&work.to_string_lossy(), &output).unwrap();

        let denied: Vec<&AppliedChange> = changes.iter().filter(|c| !c.success).collect();
        assert_eq!(denied.len(), 2, "两个越界块都应被拒: {changes:?}");
        assert!(
            denied.iter().all(|c| c
                .error
                .as_deref()
                .unwrap_or("")
                .contains("路径越界")),
            "拒绝原因要说清是路径越界: {denied:?}"
        );
        assert!(
            !outside.exists(),
            "工作目录之外的文件绝不能被创建（这是提示注入的直接后果）"
        );
        // 合规块不受影响，仍照常写入。
        assert_eq!(std::fs::read_to_string(work.join("ok.rs")).unwrap(), "fine");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_and_apply_keeps_inner_triple_backticks_intact() {
        // 围栏按「起始反引号数量」匹配：四反引号起始时，内容里的三反引号不得提前截断，
        // 否则写出的文件被砍半（Markdown/文档类文件必然中招）。
        let dir = temp_dir("apply_fence");
        let work = dir.to_string_lossy().to_string();
        let output = "````file:README.md\n\
             # Doc\n\
             ```rust\n\
             fn inner() {}\n\
             ```\n\
             tail\n\
             ````\n";

        let changes = parse_and_apply(&work, output).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].success, "{changes:?}");
        let written = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(written.contains("```rust"), "内层三反引号应完整保留: {written:?}");
        assert!(written.contains("fn inner() {}"));
        assert!(written.ends_with("tail"), "内容不得被内层围栏提前截断: {written:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_and_apply_discards_unclosed_block_instead_of_writing_truncated_file() {
        // 输出被截断（客户端断连 / max_tokens 用尽）时，绝不能把半个文件写上去覆盖用户源码。
        let dir = temp_dir("apply_unclosed");
        let work = dir.to_string_lossy().to_string();
        let target = dir.join("src/a.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "ORIGINAL").unwrap();

        let output = "```file:src/a.rs\nfn half_written() {\n";
        let changes = parse_and_apply(&work, output).unwrap();

        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "未闭合块必须判失败");
        assert!(
            changes[0].error.as_deref().unwrap_or("").contains("未正确闭合"),
            "原因要点明未闭合: {changes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "ORIGINAL",
            "原文件不得被截断内容覆盖"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_and_apply_ignores_plain_code_blocks_without_file_prefix() {
        // 决策者常在答案里贴普通代码块举例（无 file: 前缀）——不得被误当成待写文件。
        let dir = temp_dir("apply_plain");
        let work = dir.to_string_lossy().to_string();
        let output = "示例：\n```rust\nfn demo() {}\n```\n以上仅为示例。\n";

        let changes = parse_and_apply(&work, output).unwrap();
        assert!(changes.is_empty(), "普通代码块不该产生写入动作: {changes:?}");
        assert!(!dir.join("rust").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn within_work_root_resolves_links_and_fails_closed() {
        let dir = temp_dir("within_root");
        let work = dir.join("proj");
        let inside = work.join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        assert!(is_within_work_root(&work, &work), "work_dir 自身在其下");
        assert!(is_within_work_root(&work, &inside), "真实子目录应通过");
        assert!(!is_within_work_root(&work, &outside), "同级目录不在其下");
        assert!(
            !is_within_work_root(&work, dir.as_path()),
            "父目录不在其下"
        );
        // fail-closed：路径不存在 → canonicalize 失败 → 判不安全（宁可拒写让用户重试）。
        assert!(
            !is_within_work_root(&work, &work.join("does-not-exist")),
            "无法确认落点时必须判为不安全"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 符号链接逃逸：`link/` 指向工作目录之外时，`link/x` 这个相对路径不含 `..`、非绝对路径，
    /// **组件检查完全放行**，但实际写到了外面。canonicalize 二次校验就是为堵这个洞。
    ///
    /// Windows 上目录符号链接需要特权，故优先用 **junction**（`mklink /J`）——普通权限即可创建，
    /// 且 `canonicalize` 同样会解析它，能真实覆盖这条路径而不是跳过。两种都建不成才跳过。
    #[test]
    fn parse_and_apply_refuses_symlinked_dir_escaping_work_dir() {
        let dir = temp_dir("apply_symlink");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let link = work.join("link");
        let linked = link_dir_for_test(&outside, &link);
        if !linked {
            eprintln!("跳过：当前环境无法创建目录链接（symlink 需特权、junction 也失败）");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        // 前置确认：这条路径确实能过组件检查——否则本测试没在验证 canonicalize 那道防线。
        assert!(
            is_safe_relative_path("link/pwned.txt"),
            "该路径不含 ..、非绝对，组件检查必然放行；正是第二道防线的用途所在"
        );

        let output = "```file:link/pwned.txt\nHACKED\n```\n";
        let changes = parse_and_apply(&work.to_string_lossy(), output).unwrap();

        assert_eq!(changes.len(), 1);
        assert!(
            !changes[0].success,
            "经目录链接落到工作目录之外必须被拒: {changes:?}"
        );
        assert!(
            changes[0].error.as_deref().unwrap_or("").contains("工作目录之外"),
            "原因要点明真实落点越界: {changes:?}"
        );
        assert!(
            !outside.join("pwned.txt").exists(),
            "链接目标目录里绝不能被写入"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 建一个指向 `target` 的目录链接。Windows 先试 symlink（需特权），回退 junction；
    /// 其他平台用 unix symlink。返回是否建成。
    fn link_dir_for_test(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(target, link).is_ok() {
                return true;
            }
            // junction：普通权限可创建，canonicalize 同样解析。
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// 建一个指向 `target` 文件的符号链接。Windows 需特权，建不成返回 false（测试跳过）。
    fn link_file_for_test(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// 逃逸路径 ①：**多一级**路径穿透链接目录。
    ///
    /// 原实现只在「父目录已存在」时校验，而 `link/sub/x.txt` 的父目录 `link/sub` 不存在，
    /// 校验被短路跳过，紧随其后的 `create_dir_all` 沿链接把目录建到工作目录之外。
    /// 与 `..._refuses_symlinked_dir_escaping_work_dir` 的区别就是这一级之差 —— 那条恰好
    /// 命中「父目录存在」，所以旧实现能过；这条不能。
    #[test]
    fn parse_and_apply_refuses_new_subdir_through_linked_dir() {
        let dir = temp_dir("apply_symlink_deep");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let link = work.join("vendor");
        if !link_dir_for_test(&outside, &link) {
            eprintln!("跳过：当前环境无法创建目录链接");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        // 前置确认：组件检查放行，且父目录确实不存在（旧实现正是在此被短路）。
        assert!(is_safe_relative_path("vendor/sub/pwned.txt"));
        assert!(
            !work.join("vendor/sub").exists(),
            "父目录必须不存在，否则测不到「跳过校验」那条路径"
        );

        let output = "```file:vendor/sub/pwned.txt\nHACKED\n```\n";
        let changes = parse_and_apply(&work.to_string_lossy(), output).unwrap();

        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "穿透链接目录建新子目录必须被拒: {changes:?}");
        assert!(
            !outside.join("sub").exists(),
            "链接目标目录下绝不能被建出子目录"
        );
        assert!(
            !outside.join("sub/pwned.txt").exists(),
            "更不能写入内容"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 逃逸路径 ②：**目标文件自身**是符号链接。
    ///
    /// 父目录校验完全通过（就是 work_root），但 `fs::write` 跟随链接，把链接指向的
    /// 工作目录外文件整份覆盖。仓库里带一个这样的链接即可（git 能 checkout 符号链接）。
    #[test]
    fn parse_and_apply_refuses_writing_through_symlinked_file() {
        let dir = temp_dir("apply_symlink_file");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let secret = dir.join("secret.conf");
        std::fs::write(&secret, "ORIGINAL").unwrap();

        let link = work.join("notes.md");
        if !link_file_for_test(&secret, &link) {
            eprintln!("跳过：当前环境无法创建文件符号链接（Windows 需特权）");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }

        let output = "```file:notes.md\nHACKED\n```\n";
        let changes = parse_and_apply(&work.to_string_lossy(), output).unwrap();

        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "写入符号链接必须被拒: {changes:?}");
        assert!(
            changes[0].error.as_deref().unwrap_or("").contains("符号链接"),
            "原因要点明是链接: {changes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "ORIGINAL",
            "链接指向的工作目录外文件绝不能被改写"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// work_dir 本身不存在时 fail-closed（不能因为「一路向上都没有已存在祖先」而放行）。
    #[test]
    fn parse_and_apply_refuses_when_work_dir_missing() {
        let dir = temp_dir("apply_no_workdir");
        let work = dir.join("nonexistent-proj");
        // 刻意不创建 work

        let output = "```file:a.txt\nX\n```\n";
        let changes = parse_and_apply(&work.to_string_lossy(), output).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].success, "工作目录不存在时不得写盘: {changes:?}");
        assert!(!work.exists(), "更不该顺手把工作目录创建出来");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 正常路径不能被上面几道防线误伤：新建多级子目录应当照常成功。
    #[test]
    fn parse_and_apply_still_creates_nested_dirs_normally() {
        let dir = temp_dir("apply_nested_ok");
        let work = dir.join("proj");
        std::fs::create_dir_all(&work).unwrap();

        let output = "```file:src/deep/nested/mod.rs\npub fn f() {}\n```\n";
        let changes = parse_and_apply(&work.to_string_lossy(), output).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].success, "普通多级新建不得被误拒: {changes:?}");
        assert_eq!(
            std::fs::read_to_string(work.join("src/deep/nested/mod.rs")).unwrap(),
            "pub fn f() {}"
        );

        std::fs::remove_dir_all(&dir).ok();
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
            id: "k1".into(),
            category_id: CategoryType::ClaudeCli,
            name: "mock".into(),
            vendor: "test".into(),
            base_url: base_url.into(),
            protocol: Protocol::Anthropic,
            has_secret: true,
            enabled: true,
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
