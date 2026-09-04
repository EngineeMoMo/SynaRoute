//! 「检索 → 成员 → 聚合 → 决策者 → 记账」这一轮的**唯一实现**。
//!
//! # 为什么必须只有一份
//!
//! 这段骨架此前在 `run_plan`（桌面端 Phase1）与 `run_mcp`（MCP 通道）里各写了一遍，
//! 连那段 20 行的「不把 usage 交给累加器」注释都复制了两份。到 2026-09-03 复查时，
//! 两份已经漂出**四处**真缺陷 —— 每一处都是「MCP 那份对、桌面端那份漏了」：
//!
//! 1. 🔴 **桌面端决策者的 token 用量从未进累加器。** `run_plan` 拿到 `decider_used` 之后
//!    只把它拼进 detail 文本，唯一带 usage 参数的位置（那条「本次聚合合计」）刻意传 `None`，
//!    而紧挨着的注释写着「分项已各自记过账（…决策者在其调用点…）」—— 那句话在 `run_mcp`
//!    里是真的，在 `run_plan` 里**没有那个调用点**。决策者是整轮最重的一笔（prompt 内嵌
//!    全部成员答案），于是桌面端跑聚合，用量页与估算金额系统性偏低，且落进 `usage.json`
//!    日桶后**永不自愈**。
//! 2. 🔴 **`aggregate_trace_enabled` 在桌面端半失效。** 整个 `run_plan` 一次都没读那个开关
//!    —— 成员的入参出参能看到（`gather_members` 里读了），决策者的看不到。而决策者的入参
//!    才是「为什么最终答案是这样」的答案。开关开着却只生效一半，属「界面撒谎」那一类。
//! 3. **桌面端日志缺四条关键行**：`run_mcp` 会落「大脑聚合开始 / 参与者汇总 / 交由决策者 /
//!    决策者返回」，`run_plan` 全都没有。同一个功能，从桌面端跑和从 MCP 跑，排障信息量
//!    差一大截，而没有任何注释说这是刻意的。
//! 4. **独答降级的说明事件**只有 MCP 那份有。桌面端用户看到一段单模型答案，日志里没有
//!    任何一行解释「为什么这次只有一个模型」。
//!
//! 也就是说：**两份实现里较新的那些修复只落在被人盯着的那一份上。** 这正是本仓
//! 「两处各写一份必然漂移」那条纪律的教科书形态，只是它藏在同一个文件里、隔了 300 行。
//!
//! # 差异怎么表达
//!
//! 两条路径**真正的**差异只有两处：决策者 prompt 的措辞，以及决策者失败时那句降级说明。
//! 用 [`RoundKind`] 承载，不用函数指针 —— 加第三种调用方时，编译器会要求它在两处 match
//! 里都表态，而函数指针只会让人漏掉一处。
//!
//! 其余「差异」都是伪差异：图片只有 MCP 会传（桌面端给空切片，形状完全一致）、
//! cwd 优先级在调用方解析完再进来、返回结构由调用方自己投影。

use super::*;

/// 这一轮是谁在跑。决定决策者 prompt 的措辞与失败降级的说明文案。
pub(crate) enum RoundKind {
    /// 桌面端 Phase1：只出**修改计划**，下游有「确认执行」会真的写文件。
    DesktopPlan,
    /// MCP 通道：出最终答案，文件修改由客户端（Codex / Claude Code）自己执行。
    Mcp,
}

impl RoundKind {
    /// 决策者 prompt。`file_section` 已是拼好的「## 相关文件」段（可能为空串）。
    fn decider_prompt(&self, prompt: &str, aggregated: &str, file_section: &str) -> String {
        match self {
            Self::DesktopPlan => format!(
                "你是最终决策者。以下是多位代码审阅者的分析意见。\n\
                 请综合所有意见，输出一份具体的修改计划：列出需要修改的文件路径和具体改动描述。\n\
                 注意：现在只输出计划，不要输出代码。等用户确认后再执行。\n\n\
                 ## 用户需求\n{prompt}\n\n\
                 ## 审阅意见\n{aggregated}\n\n\
                 {file_section}\
                 请输出修改计划："
            ),
            Self::Mcp => format!(
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
                 请输出你的综合答案："
            ),
        }
    }

    /// 决策者失败时，返回给用户的那段头部说明里「这堆东西是什么」那一句。
    ///
    /// 🔴 **两条路径不能共用一句**：桌面端下游有「确认执行」按钮会真的写文件，
    /// 必须明说这不是可执行计划；而 MCP 路径压根没有那个按钮，对它说「请勿直接确认执行」
    /// 是指向一个不存在的操作 —— 同本仓「指错方向的提示比没有提示更糟」那条。
    fn failure_note(&self) -> &'static str {
        match self {
            Self::DesktopPlan => "（**不是可执行计划**，请勿直接确认执行）",
            Self::Mcp => "，供你参考",
        }
    }
}

/// 一轮聚合的入参。工作目录由调用方解析完再进来 —— MCP 的 `cwd` 优先级与桌面端的
/// auto-follow 是两条不同的解析规则，放进来会让本模块替调用方做决定。
pub(crate) struct RoundSpec<'a> {
    pub kind: RoundKind,
    pub prompt: &'a str,
    /// MCP 的 `images` 参数加载结果。桌面端恒为空切片 —— 形状与带图完全一致，不是分支。
    pub images: &'a [ImagePart],
    pub work_dir: Option<String>,
}

/// 一轮聚合的产物。桌面端只用 `analysis` + `work_dir`，MCP 全都要（组装 Markdown 页脚）。
pub struct RoundOutcome {
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

/// 跑一轮聚合。**桌面端 Phase1 与 MCP 通道共用这一条路径。**
pub(crate) async fn run(
    store: &Arc<Store>,
    category: CategoryType,
    brain: &BrainConfig,
    spec: RoundSpec<'_>,
) -> AppResult<RoundOutcome> {
    let decider_ref = brain
        .decider_ref
        .clone()
        .ok_or_else(|| AppError::Invalid("未配置最终决策者".into()))?;

    // 整轮墙钟 deadline：串行的「成员 → 压缩 → 决策者」共享同一预算，各阶段按剩余时间递减
    // 分配，并给决策者留保底（见 decider_floor_ms）。总量始终压在客户端 MCP 超时之下，
    // 保证服务端能在客户端杀连接前优雅降级返回。
    let deadline = std::time::Instant::now() + Duration::from_millis(brain.total_timeout_ms);
    let decider_floor = decider_floor_ms(brain.total_timeout_ms);
    let remaining_ms = |d: std::time::Instant| {
        d.saturating_duration_since(std::time::Instant::now()).as_millis() as u64
    };
    let effective_work_dir = spec.work_dir;
    let prompt = spec.prompt;

    // 0. 文件检索
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
    let file_section = if file_context.is_empty() {
        String::new()
    } else {
        format!("## 相关文件\n{file_context}\n\n")
    };

    // 成员清单（只展示配置里的参与者，固定调用，不做故障转移换 Key）
    let member_plan: Vec<String> = brain
        .members
        .iter()
        .map(|m| {
            let name = store
                .get_key(&m.key_id)
                .map(|k| k.name)
                .unwrap_or_else(|| m.key_id.clone());
            format!("{name}/{}", m.model_name)
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
    let members_budget =
        upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
    let tool_env = prepare_tool_env(store, category, brain, effective_work_dir.as_deref()).await;
    let gathered = gather_members(
        store,
        category,
        brain,
        &member_prompt,
        spec.images,
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
        // 🔴 `with_usage` 不能省：独答同样烧额度，而这是**两份旧实现各漏一半**的那一处 ——
        // 桌面端记了这笔、MCP 路径裸 `call_ref` 没记。合并之后两条都对。
        let (fallback, solo_used) =
            upstream::with_usage(call_ref(store, category, &decider_ref, &fallback_prompt, solo_budget)).await;
        let fallback = fallback?;
        store.append_event_full(
            category,
            "aggregate",
            ref_key_id(&decider_ref),
            &format!(
                "独答降级 · {}{}",
                label_ref(store, &decider_ref),
                if solo_used.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", solo_used.fmt_compact())
                }
            ),
            None,
            None,
            (!solo_used.is_empty()).then_some(solo_used),
        );
        return Ok(RoundOutcome {
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
            let compress_budget =
                upstream_phase_budget_ms(remaining_ms(deadline), decider_floor, PHASE_MIN_BUDGET_MS);
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

    // 4. 决策者综合
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
    let decider_prompt = spec.kind.decider_prompt(prompt, &aggregated, &file_section);
    let decider_started = std::time::Instant::now();
    // 决策者阶段：整轮剩余时间全给它（保底 decider_floor 已在前面阶段被保护）。
    let decider_budget = decider_phase_budget_ms(remaining_ms(deadline), PHASE_MIN_BUDGET_MS);
    let (decider_res, decider_used) = upstream::with_usage(call_ref(
        store,
        category,
        &decider_ref,
        &decider_prompt,
        decider_budget,
    ))
    .await;
    let trace_on = store.get_settings().aggregate_trace_enabled;
    let analysis = match decider_res {
        Ok(a) => {
            let latency = decider_started.elapsed().as_millis() as u64;
            // 🔴 这一条同时补上两处旧漂移：桌面端**既不落这条事件、也不把 usage 交给累加器**
            // （见模块头 1、2）。带 trace 时展开可见「喂给决策者的完整入参 + 它的最终答案」。
            store.append_event_full(
                category,
                "aggregate",
                ref_key_id(&decider_ref),
                &format!(
                    "决策者返回 · {} · {latency}ms{}",
                    label_ref(store, &decider_ref),
                    if decider_used.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", decider_used.fmt_compact())
                    }
                ),
                trace_on
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
            // 失败**也烧了额度**（尤其超时前已经把整份 prompt 发出去了），故这一条也带 usage。
            store.append_event_full(
                category,
                "aggregate",
                ref_key_id(&decider_ref),
                &format!(
                    "决策者失败 · {} · {e} · 降级为「返回已聚合成员意见」{}",
                    label_ref(store, &decider_ref),
                    if decider_used.is_empty() {
                        String::new()
                    } else {
                        format!(" · 已消耗 {}", decider_used.fmt_compact())
                    }
                ),
                trace_on
                    .then(|| {
                        trace_for_ref(
                            store,
                            &decider_ref,
                            &decider_prompt,
                            &e.to_string(),
                            latency,
                            false,
                        )
                    })
                    .flatten(),
                None,
                (!decider_used.is_empty()).then_some(decider_used),
            );
            format!(
                "> ⚠️ 决策者 `{}` 未能完成综合分析：{e}\n\
                 > 以下是已成功获取的 {} 位专家意见{}：\n\n{}",
                label_ref(store, &decider_ref),
                answers.len(),
                spec.kind.failure_note(),
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
            // （成员在 gather_members、决策者在上面那个 match 的两个分支、压缩阶段自己记）。
            // append_event_full 对**任何**带 usage 的事件都无条件累加进 usage_totals，
            // 汇总行再带一次 = 用量面板与估算金额恒为真实值的 2 倍，且会落进 usage.json 日桶、
            // 重启后依旧翻倍、永不自愈。token 数已写在上面的 detail 文本里，展示不受影响。
            None,
        );
    }

    Ok(RoundOutcome {
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
