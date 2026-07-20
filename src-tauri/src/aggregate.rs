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
use crate::model::{AggregateMode, AggregateResult, AppliedChange, BrainConfig, CategoryType};
use crate::retrieval;
use crate::store::Store;
use crate::upstream;
use futures_util::future::join_all;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

struct MemberAnswer {
    label: String,
    answer: String,
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
        let fallback = call_ref(store, &decider_ref, prompt).await?;
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
            match compress(store, &summarizer_ref, answers).await {
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
    let plan = call_ref(store, &decider_ref, &plan_prompt).await?;
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

    let result = call_ref(store, &decider_ref, &exec_prompt).await?;

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

    // 0. 文件检索
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

    // 1. 参与者并行分析（只读）
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
        let fallback = call_ref(store, &decider_ref, prompt).await?;
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
        AggregateMode::Full => build_full_context(&answers),
        AggregateMode::Compressed => {
            let summarizer_ref = brain
                .summarizer_ref
                .clone()
                .unwrap_or_else(|| decider_ref.clone());
            match compress(store, &summarizer_ref, &answers).await {
                Ok(s) => s,
                // 压缩失败（summarizer Key 限流/余额/被删）不作废已成功的成员回答，
                // 降级为全量拼接，避免整轮聚合白跑。
                Err(e) => {
                    tracing::warn!("compress 失败，降级为全量上下文: {e}");
                    build_full_context(&answers)
                }
            }
        }
    };

    // 4. 决策者综合 → 建议 / 修改计划
    //    强调「只出方案，不写代码」——由客户端（Codex/Claude Code）执行文件修改（Q5）。
    let decider_prompt = format!(
        "你是最终决策者。以下是多位代码审阅者对同一需求的分析意见。\n\
         请综合所有意见，输出一份清晰、可执行的方案或修改计划。\n\
         要求：\n\
         - 用 Markdown 组织，先给结论，再给要修改的文件路径与具体改动说明\n\
         - 涉及代码时可给出关键片段示例，但不要臆造完整文件\n\
         - 标注审阅者之间的分歧点（如有）\n\
         - 你只负责出方案，实际的文件修改会由用户在客户端确认后执行\n\n\
         ## 用户需求\n{prompt}\n\n\
         ## 审阅意见\n{aggregated}\n\n\
         {file_section}\
         请输出你的综合方案：",
        prompt = prompt,
        aggregated = aggregated,
        file_section = if file_context.is_empty() {
            String::new()
        } else {
            format!("## 相关文件\n{}\n\n", file_context)
        }
    );
    let analysis = call_ref(store, &decider_ref, &decider_prompt).await?;

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
        "你是一位代码审阅者。请分析以下需求和相关文件，给出你的修改建议。\n\
         重要：你只能阅读和分析，不能执行任何修改。请输出：\n\
         1. 你对需求的理解\n\
         2. 需要修改哪些文件、为什么\n\
         3. 具体的修改建议\n\n\
         ## 用户需求\n{prompt}{file_section}"
    )
}

/// 单个成员任务的结果：成功带答案，失败带原因（用于落日志）。
enum MemberOutcome {
    Ok(MemberAnswer),
    /// 调用失败：label + 具体原因（超时 / HTTP / 连接 / 空答案）。
    Failed { label: String, reason: String },
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
    let retry = store.get_settings().upstream_retry_enabled;
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
            if !crate::health::is_candidate(&key.health) {
                return MemberOutcome::Unavailable {
                    label,
                    reason: "熔断中或健康检查判定不可用".into(),
                };
            }
            let Some(secret) = store.secrets.read().get(&key_id).ok().flatten() else {
                return MemberOutcome::Unavailable {
                    label,
                    reason: "未配置密钥".into(),
                };
            };
            let max_tokens = key.params.max_tokens.unwrap_or(4096);
            // 仅对实际模型调用套超时（这才是「成员自己的工作时间」）。
            let call = upstream::text_completion(&key, &secret, &model, &prompt, max_tokens, retry);
            match timeout(total_timeout, call).await {
                Ok(Ok(ans)) if !ans.trim().is_empty() => {
                    MemberOutcome::Ok(MemberAnswer { label, answer: ans })
                }
                Ok(Ok(_)) => MemberOutcome::Failed {
                    label,
                    reason: "返回空答案".into(),
                },
                Ok(Err(e)) => MemberOutcome::Failed {
                    label,
                    // upstream 错误已含 HTTP 状态码 / 连接失败详情。
                    reason: format!("调用失败：{e}"),
                },
                Err(_) => MemberOutcome::Failed {
                    label,
                    reason: format!("超时（>{}ms）", brain.total_timeout_ms),
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
            MemberOutcome::Ok(ans) => {
                attempted += 1;
                answers.push(ans);
            }
            MemberOutcome::Failed { label, reason } => {
                attempted += 1;
                failed += 1;
                // 失败原因落日志（归「大脑聚合」分组），不再静默吞。
                store.append_event(
                    category,
                    "aggregate",
                    None,
                    &format!("参与者失败 · {label} · {reason}"),
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
    summarizer_ref: &str,
    answers: &[MemberAnswer],
) -> AppResult<String> {
    let mut joined = String::new();
    for (i, a) in answers.iter().enumerate() {
        joined.push_str(&format!(
            "\n【审阅者{} · {}】\n{}\n",
            i + 1,
            a.label,
            a.answer
        ));
    }
    let sum_prompt = format!(
        "以下是多位代码审阅者对同一需求的分析和建议。\n\
         请提炼各位的关键要点、共识与分歧，压缩成简洁的要点清单，供最终决策参考。\n\n{joined}"
    );
    call_ref(store, summarizer_ref, &sum_prompt).await
}

fn build_full_context(answers: &[MemberAnswer]) -> String {
    let mut s = String::new();
    for (i, a) in answers.iter().enumerate() {
        s.push_str(&format!(
            "\n【审阅者{} · {}】\n{}\n",
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

async fn call_ref(store: &Arc<Store>, reference: &str, prompt: &str) -> AppResult<String> {
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
    upstream::text_completion(&key, &secret, model, prompt, max_tokens, retry).await
}
