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
    })
}

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
}

/// gather_members 的结果：成功答案 + 统计（供结果面板展示「N 参与 / M 失败」）。
struct GatherOutcome {
    answers: Vec<MemberAnswer>,
    /// 实际发起调用的成员数（不含被禁用而跳过的）。
    attempted: usize,
    /// 调用失败 / 不可用的成员数。
    failed: usize,
    /// 因 Key 被禁用而跳过的成员数。
    skipped_disabled: usize,
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

    // 0. 文件检索（work_dir 若开启自动跟随则取最新活跃项目）
    let effective_work_dir = resolve_work_dir(&brain);
    let file_context = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let max_tokens = brain.max_context_tokens;
            let files = retrieval::retrieve(work_dir, prompt, max_tokens)
                .await
                .unwrap_or_default();
            format_file_context(&files)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // 1. 构建参与者 prompt（只读角色）
    let member_prompt = build_member_prompt(prompt, &file_context);

    // 2. 并行成员解答
    let gathered = gather_members(store, category, &brain, &member_prompt).await;
    let answers = &gathered.answers;

    if answers.is_empty() {
        let fallback_prompt = build_solo_decider_prompt(prompt, &file_context);
        let fallback = call_ref(store, &decider_ref, &fallback_prompt, brain.total_timeout_ms).await?;
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
            match compress(store, category, &summarizer_ref, answers, brain.total_timeout_ms).await {
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
    let plan = call_ref(store, &decider_ref, &plan_prompt, brain.total_timeout_ms).await?;
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
    let effective_work_dir = match pinned_work_dir {
        Some(d) if !d.trim().is_empty() => Some(d),
        _ => resolve_work_dir(&brain),
    };

    // 重新获取文件上下文
    let file_context = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            let files = retrieval::retrieve(work_dir, prompt, brain.max_context_tokens).await.unwrap_or_default();
            format_file_context(&files)
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

    let result = call_ref(store, &decider_ref, &exec_prompt, brain.total_timeout_ms).await?;

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
    /// 实际发起调用的成员数（不含被禁用而跳过的）
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

    // 工作目录优先级：MCP 显式 cwd > brain 配置（auto-follow / 手工 work_dir）
    let effective_work_dir = match cwd {
        Some(c) if !c.trim().is_empty() => Some(c),
        _ => resolve_work_dir(&brain),
    };

    // 文件检索
    let files = if brain.retrieval_enabled {
        if let Some(ref work_dir) = effective_work_dir {
            retrieval::retrieve(work_dir, prompt, brain.max_context_tokens)
                .await
                .unwrap_or_default()
        } else {
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
    let member_prompt = build_member_prompt(prompt, &file_context);
    let gathered = gather_members(store, category, &brain, &member_prompt).await;
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
            "参与者汇总 · 发起 {} · 成功 {} · 失败 {} · 禁用跳过 {}",
            members_attempted,
            answers.len(),
            members_failed,
            members_skipped_disabled
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
        let fallback = call_ref(store, &decider_ref, &fallback_prompt, brain.total_timeout_ms).await?;
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
            match compress(store, category, &summarizer_ref, answers, brain.total_timeout_ms).await {
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
    let analysis = match call_ref(store, &decider_ref, &decider_prompt, brain.total_timeout_ms).await {
        Ok(a) => {
            let latency = decider_started.elapsed().as_millis() as u64;
            // 带 trace：展开可见「喂给决策者的完整入参（原问题+聚合意见+文件）+ 决策者最终答案」。
            store.append_event_trace(
                category,
                "aggregate",
                None,
                &format!("决策者返回 · {} · {latency}ms", label_ref(store, &decider_ref)),
                store.get_settings().aggregate_trace_enabled
                    .then(|| trace_for_ref(store, &decider_ref, &decider_prompt, &a, latency, true))
                    .flatten(),
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
/// 若 auto_follow_active 为真，则从各工具会话历史中挑选最近活跃的目录；
/// 否则使用用户在配置中手工填写的 work_dir。
fn resolve_work_dir(brain: &BrainConfig) -> Option<String> {
    if brain.auto_follow_active {
        if let Ok(list) = crate::workdirs::scan() {
            if let Some(first) = list.into_iter().next() {
                return Some(first.path);
            }
        }
    }
    brain.work_dir.clone()
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
    /// 被禁用而跳过（不计失败）。
    SkippedDisabled,
    /// 无密钥 / 熔断 / Key 不存在等前置不可用（计入失败，附原因）。
    Unavailable { label: String, reason: String },
}

async fn gather_members(
    store: &Arc<Store>,
    category: CategoryType,
    brain: &BrainConfig,
    prompt: &str,
) -> GatherOutcome {
    let total_timeout = Duration::from_millis(brain.total_timeout_ms);
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

    // 超时按「单个成员的实际模型调用」计。信号量排队时间**不**计入超时——否则
    // concurrency_limit 小于成员数时，后排成员在队列里就耗尽预算、从未发出请求即被判超时。
    // 故先 acquire permit（排队，不设时限），再对真正的模型调用套 timeout。
    // 慢成员到点各自作废，不拖垮已答完的成员。失败不再静默：各自返回具体原因，由下方落日志。
    let tasks = brain.members.iter().map(|m| {
        let store = store.clone();
        let sem = sem.clone();
        let key_id = m.key_id.clone();
        let model = m.model_name.clone();
        let prompt = prompt.clone();
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
                return MemberOutcome::SkippedDisabled;
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
            let Some(secret) = store.secrets.read().get(&key_id).ok().flatten() else {
                return MemberOutcome::Unavailable {
                    label,
                    reason: "未配置密钥".into(),
                };
            };
            let max_tokens = key.params.max_tokens.unwrap_or(4096);
            // 仅对实际模型调用套超时（这才是「成员自己的工作时间」）。
            // 单请求 HTTP 超时给 brain 预算 +5s 余量（此前误用 key 的 30s 代理级超时，
            // 非流式长回答必然被掐死、重试 3 次 ≈ 91s 全灭）；外层 tokio timeout 先到点，
            // 报出干净的「超时（>Xms）」而非 reqwest 的晦涩错误。
            let req_timeout = Duration::from_millis(brain.total_timeout_ms.saturating_add(5_000));
            let started = std::time::Instant::now();
            let mk_meta = |latency_ms: u64| MemberCallMeta {
                key_name: key.name.clone(),
                vendor: key.vendor.clone(),
                protocol: key.protocol,
                base_url: key.base_url.clone(),
                model: model.clone(),
                latency_ms,
            };
            let call = upstream::text_completion(&key, &secret, &model, &prompt, max_tokens, retry, req_timeout);
            match timeout(total_timeout, call).await {
                Ok(Ok(ans)) if !ans.trim().is_empty() => {
                    let meta = mk_meta(started.elapsed().as_millis() as u64);
                    MemberOutcome::Ok(MemberAnswer { label, answer: ans }, meta)
                }
                Ok(Ok(_)) => MemberOutcome::Failed {
                    label,
                    reason: "返回空答案".into(),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64)),
                },
                Ok(Err(e)) => MemberOutcome::Failed {
                    label,
                    // upstream 错误已含 HTTP 状态码 / 连接失败详情。
                    reason: format!("调用失败：{e}"),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64)),
                },
                Err(_) => MemberOutcome::Failed {
                    label,
                    reason: format!("超时（>{}ms）", brain.total_timeout_ms),
                    meta: Some(mk_meta(started.elapsed().as_millis() as u64)),
                },
            }
        }
    });

    let outcomes = join_all(tasks).await;

    let mut answers = Vec::new();
    let mut attempted = 0usize;
    let mut failed = 0usize;
    let mut skipped_disabled = 0usize;
    for o in outcomes {
        match o {
            MemberOutcome::Ok(ans, meta) => {
                attempted += 1;
                // 带 trace：展开可见「喂给该成员的完整 prompt + 成员的完整答案」。受开关控制。
                let trace = trace_enabled.then(|| RequestTrace {
                    key_name: meta.key_name,
                    vendor: meta.vendor,
                    protocol: meta.protocol,
                    url: meta.base_url,
                    requested_model: meta.model.clone(),
                    real_model: meta.model,
                    request_body: cap_text(&prompt),
                    response_body: cap_text(&ans.answer),
                    status: None,
                    latency_ms: meta.latency_ms,
                    ok: true,
                });
                store.append_event_trace(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者成功 · {} · {}ms", ans.label, meta.latency_ms),
                    trace,
                );
                answers.push(ans);
            }
            MemberOutcome::Failed { label, reason, meta } => {
                attempted += 1;
                failed += 1;
                // 失败原因落日志（归「大脑聚合」分组），不再静默吞；带 trace 供展开看入参。受开关控制。
                let trace = meta.filter(|_| trace_enabled).map(|m| RequestTrace {
                    key_name: m.key_name,
                    vendor: m.vendor,
                    protocol: m.protocol,
                    url: m.base_url,
                    requested_model: m.model.clone(),
                    real_model: m.model,
                    request_body: cap_text(&prompt),
                    response_body: cap_text(&reason),
                    status: None,
                    latency_ms: m.latency_ms,
                    ok: false,
                });
                store.append_event_trace(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者失败 · {label} · {reason}"),
                    trace,
                );
            }
            MemberOutcome::Unavailable { label, reason } => {
                failed += 1;
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者不可用 · {label} · {reason}"),
                );
            }
            MemberOutcome::SkippedDisabled => {
                skipped_disabled += 1;
            }
        }
    }

    GatherOutcome {
        answers,
        attempted,
        failed,
        skipped_disabled,
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
    let result = call_ref(store, summarizer_ref, &sum_prompt, budget_ms).await?;
    let latency = started.elapsed().as_millis() as u64;
    // 带 trace：展开可见「喂给汇总者的全部成员答案 + 压缩后的要点清单」。受开关控制。
    let trace = if store.get_settings().aggregate_trace_enabled {
        trace_for_ref(store, summarizer_ref, &sum_prompt, &result, latency, true)
    } else {
        None
    };
    store.append_event_trace(
        category,
        "aggregate",
        None,
        &format!("汇总成功 · {} · {latency}ms", label_ref(store, summarizer_ref)),
        trace,
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
fn is_safe_relative_path(path: &str) -> bool {
    use std::path::Component;
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
    // 组件里不能有空片段/纯 .（正常相对路径不含），且非空
    !path.trim().is_empty()
}

/// 解析决策者输出中的 ```file:path 代码块并写入磁盘。
///
/// 安全与健壮性（本轮修复）：
/// - 路径遏制：拒绝 `../`、绝对路径、盘符/UNC，防止 LLM 输出逃逸 work_dir 写任意文件（存在提示注入面）。
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

            let full_path = work_root.join(&path);
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
    let secret = store
        .secrets
        .read()
        .get(key_id)?
        .ok_or_else(|| AppError::Invalid("决策者密钥缺失".into()))?;
    let max_tokens = key.params.max_tokens.unwrap_or(4096);
    let retry = store.get_settings().upstream_retry_enabled;
    // 大脑聚合路径：固定打该引用对应的 Key+模型，不走代理故障转移。
    // 上游瞬时错误仍可按设置做同 Key 重试（upstream_retry），但绝不会换成别的 Key。
    // 单请求 HTTP 超时同样给预算 +5s 余量（勿用 key 的 30s 代理级超时，见 gather_members 注释）。
    let req_timeout = Duration::from_millis(budget_ms.saturating_add(5_000));
    let call = upstream::text_completion(&key, &secret, model, prompt, max_tokens, retry, req_timeout);
    let result = match timeout(Duration::from_millis(budget_ms), call).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(AppError::Upstream(format!(
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
        return Err(AppError::Upstream(format!(
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
}
