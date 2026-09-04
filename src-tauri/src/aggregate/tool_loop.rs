//! 成员的**工具循环**：一次调用里模型可以反复「读文件 → 再读 → 出结论」。
//!
//! 从 `aggregate.rs` 抽出来的（那边顶在棘轮上），而它本来就是个独立关注点：
//! 四条护栏（轮数 / 时间预算 / 工具失败不中断 / 收尾轮仍声明 tools）、单轮宽度上限、
//! 分批并发执行、历史膨胀裁剪 —— 每一条都带着自己那段「为什么」。
//!
//! 开关关闭时（`tool_env == None`）这里恒走单轮分支，请求形状与旧的
//! `text_completion` 逐字段一致 —— 有一条回归判据钉住那一点。

use super::*;

/// 工具循环里探索阶段可用的预算占比；剩下的留给「强制出结论」那一轮。
///
/// 不留这一份，最后一次调用会在刚发出时被外层 timeout 掐掉 —— 前面几轮挖到的信息全白费，
/// 用户看到的只是「超时」。
pub(super) const TOOL_LOOP_EXPLORE_PCT: u64 = 70;

/// 工具轮数的运行时上下限。上限防配置写了个 999 把预算烧光；下限保证「开了工具至少能真调一次」
/// （只给 1 轮时收尾逻辑会立刻生效，等于开了开关却没有工具）。
pub(super) const TOOL_ROUNDS_RANGE: (u32, u32) = (2, 12);

/// 工具历史字符预算的 clamp 区间。
/// 下限 8000 = 一次工具结果的上限（RESULT_CHAR_CAP），再小就等于连一条完整结果都留不住；
/// 上限 400000（约 20 万 token）够极端场景，再大就失去「控膨胀」的意义。
pub(super) const TOOL_CTX_BUDGET_RANGE: (usize, usize) = (8_000, 400_000);

/// **单轮**内实际执行的工具调用数上限。超出的回一条 is_error 结果但不执行（不起子进程、不读盘）。
///
/// 为什么需要：模型一轮可以返回任意多个 tool_use（协议无上界）。一份被提示注入的文件能诱导它
/// 一轮返回上百个 → 上百次串行子进程 + 上百条 8000 字符结果塞进历史 → 下一轮重发完整历史 →
/// token 爆炸。轮数上限只管轮数、不管单轮宽度，这条补上宽度。16 对正常"并行读几个文件"够用。
pub(super) const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// 一轮里**同时**在跑的工具数。与上面那条是两回事：那条管「一轮最多执行几个」，
/// 这条管「同时几个」。
///
/// 为什么不让 16 个全并发：`grep` 与 `codegraph_query` 都起子进程，在大仓库上单个就吃满 IO，
/// 而这跑在**用户自己的开发机**上。收益早在前几个就拿到了 —— 模型一轮真正会读的文件
/// 通常 2~5 个，4 路已经把「并行读几个文件」这个主场景的墙钟压到接近单个的耗时。
pub(super) const MAX_CONCURRENT_TOOLS: usize = 4;

/// 本次成员调用实际跑几轮。
///
/// 无工具时恒为 1（形状与旧的单轮补全完全一致）；有工具时把配置值夹进
/// [`TOOL_ROUNDS_RANGE`] —— 夹下限是关键：配 1 轮会让第一轮就走收尾分支，
/// 工具声明了却永远调不动，正是「开关开着但功能没生效」那类静默失效。
pub(super) fn effective_rounds(has_tools: bool, configured: u32) -> u32 {
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
pub(super) async fn run_member_turns(
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
                // 本轮要真跑的那些**分批并发**执行，超限的直接给一条 is_error 结果、不执行。
                //
                // 为什么可以并发：这组工具全部**只读**（read_file / grep / list_dir /
                // codegraph_query），彼此无副作用、无共享可变状态（`ToolEnv` 里只有
                // work_dir / codegraph 程序名 / 字符上限三个不可变字段），而它们各自都要么
                // 起子进程（rg / codegraph）要么读盘 —— 串行时一轮的墙钟是逐个相加，
                // 而模型「并行读 5 个文件」正是最常见的形态。
                //
                // 🔴 **为什么分批而不是让 16 个全并发**：`grep` 与 `codegraph_query` 都起
                // 子进程，在大仓库上单个就吃满 IO。一轮放 16 个并发 `rg` 出去，是在**用户
                // 自己的开发机**上制造资源尖峰，而收益早在前几个就拿到了（模型一轮真正会读的
                // 文件通常 2~5 个）。
                //
                // 🔴 **为什么用 `chunks` + `join_all`，不用 `StreamExt::buffered`**：
                // 后者更精确（滑动窗口而非批屏障），但它把这些借用了 `calls` 的 future 塞进
                // `FuturesOrdered`，推导出的 auto-trait bound 不够 general —— 实测会让
                // **`mcp.rs` 里两处 `tokio::spawn` 编译失败**（`Send` is not general enough），
                // 而那两处与本改动毫无关系。批屏障的代价（一批里最慢的拖住下一批）在 4 个一批、
                // 正常 1~2 批的量级上可以忽略；让一个无关模块编译不过不行。
                //
                // 🔴 **顺序必须保持**：两家协议都要求 tool_result 与 tool_use **一一对应**，
                // 顺序错了上游直接 400。`join_all` 按入参顺序返回、批也按序拼接，故这条成立；
                // 换成 `FuturesUnordered` / `buffer_unordered`（按完成顺序）会静默打乱配对。
                let mut results = Vec::with_capacity(calls.len());
                for batch in calls
                    .iter()
                    .enumerate()
                    .collect::<Vec<_>>()
                    .chunks(MAX_CONCURRENT_TOOLS)
                {
                    let done = join_all(batch.iter().map(|(idx, c)| async move {
                        if *idx >= MAX_TOOL_CALLS_PER_TURN {
                            // 超限：回一条错误结果，不执行（不起子进程、不读盘、不占字符预算）。
                            // 模型据此在下一轮减少调用数。
                            return crate::agent_tools::over_limit_result(
                                c,
                                MAX_TOOL_CALLS_PER_TURN,
                            );
                        }
                        crate::agent_tools::execute(env, c).await
                    }))
                    .await;
                    results.extend(done);
                }
                // 事件在**结果收齐之后**按原顺序补落：并发执行时若在各自的 future 里落事件，
                // 日志顺序会随完成快慢抖动，而折叠键靠「相邻同名」生效 —— 抖动会让本该折叠的
                // 两条分开、不该折叠的挨到一起。这里按 calls 的顺序走一遍，顺序恒定。
                for (c, r) in calls.iter().zip(results.iter()) {
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
pub(super) fn tool_call_brief(c: &upstream::ToolInvocation) -> String {
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
    // 排序让同一组参数恒得同一行文本。**与折叠键无关** —— 那个键是 `tool:{label}:{工具名}`、
    // 不含参数（原注释说「折叠 key 才不会抖动」，是错的）。它守的是折叠时的 detail 覆盖：
    // 同名工具连续命中时最后一条的文本会盖掉前面，参数顺序抖动会让同一次调用显示成两种样子。
    parts.sort();
    let s = parts.join(" ");
    if s.chars().count() > 80 {
        format!("{}…", s.chars().take(80).collect::<String>())
    } else {
        s
    }
}
