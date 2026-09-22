//! 上游拒绝后的有限整流。预算整流作用于转换后的请求体，同 Key 最多重发一次。
use crate::model::{CategoryType, ProviderKey, Protocol};
use crate::store::Store;
use serde_json::Value;

pub(crate) fn is_signature_rejection(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("thinking_signature_invalid")
        || (e.contains("thinking") && e.contains("signature") && e.contains("invalid"))
        || (e.contains("thought signature") && (e.contains("not valid") || e.contains("invalid")))
        || (e.contains("signature") && (e.contains("extra inputs are not permitted") || e.contains("field required")))
        || e.contains("must start with a thinking block")
        || (e.contains("expected") && e.contains("thinking") && e.contains("found") && e.contains("tool_use"))
        || (e.contains("thinking") && e.contains("cannot be modified"))
}

const ANTHROPIC_MIN_THINKING_BUDGET: u64 = 1024;
fn is_budget_too_small(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    (e.contains("budget_tokens") || e.contains("budget tokens")) && e.contains("thinking")
        && (e.contains("greater than or equal to 1024") || e.contains(">= 1024")
            || e.contains("at least 1024") || (e.contains("1024") && e.contains("input should be")))
}
fn is_budget_exceeds_max_tokens(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    (e.contains("budget_tokens") || e.contains("budget tokens")) && e.contains("less than") && e.contains("max_tokens")
}
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Removed {
    pub thinking_blocks: usize,
    pub signature_fields: usize,
    pub thinking_disabled: bool,
}
impl Removed {
    fn any(&self) -> bool { self.thinking_blocks > 0 || self.signature_fields > 0 || self.thinking_disabled }
}
fn is_thinking(b: &Value) -> bool {
    matches!(b.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking"))
}
fn has_thinking(payload: &Value) -> bool {
    payload.get("messages").and_then(Value::as_array).into_iter().flatten().any(|m| {
        m.get("content").and_then(Value::as_array).is_some_and(|a| a.iter().any(is_thinking))
    })
}
/// 不删除仅由思考块构成的消息，避免制造空 content；保留块的 signature 也不删除。
pub(crate) fn strip_thinking_blocks(payload: &mut Value) -> Removed {
    let mut out = Removed::default();
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        for msg in messages {
            let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else { continue };
            let hits = content.iter().filter(|b| is_thinking(b)).count();
            if hits > 0 && hits < content.len() {
                content.retain(|b| !is_thinking(b));
                out.thinking_blocks += hits;
            }
            for block in content.iter_mut().filter(|b| !is_thinking(b)) {
                if let Some(obj) = block.as_object_mut() {
                    out.signature_fields += usize::from(obj.remove("signature").is_some());
                }
            }
        }
    }
    if out.any() && !has_thinking(payload) {
        if let Some(obj) = payload.as_object_mut() { out.thinking_disabled = obj.remove("thinking").is_some(); }
    }
    out
}
pub(crate) fn rectify_on_signature_error(
    err: &str, payload: &mut Value, store: &Store, category: CategoryType, key: &ProviderKey,
) -> bool {
    if !is_signature_rejection(err) { return false; }
    let mut removed = strip_thinking_blocks(payload);
    // 没有历史思考块的工具续接，同样可能被“必须以 thinking 开头”拒绝。
    let e = err.to_ascii_lowercase();
    let prefix_error = e.contains("must start with a thinking block")
        || (e.contains("expected") && e.contains("thinking") && e.contains("found") && e.contains("tool_use"));
    if prefix_error && !has_thinking(payload)
        && payload.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled") {
        if let Some(obj) = payload.as_object_mut() { removed.thinking_disabled |= obj.remove("thinking").is_some(); }
    }
    if !removed.any() { return false; }
    store.append_event(category, "failover", Some(&key.id), &format!(
        "已自动摘除扩展思考块后重试 · {} · 移除思考块 {} 个、残留签名 {} 处{}；不保证上游接受重试",
        key.name, removed.thinking_blocks, removed.signature_fields,
        if removed.thinking_disabled { "、本轮已降级为不开思考" } else { "" },
    ));
    true
}
/// 仅修正明确的请求校验错误；不抬 max_tokens，不主动开启或改写 adaptive thinking。
pub(crate) fn rectify_on_budget_error(
    status: u16, err: &str, payload: &mut Value, store: &Store, category: CategoryType, key: &ProviderKey,
) -> bool {
    if !matches!(status, 400 | 422) || key.protocol != Protocol::Anthropic { return false; }
    let raise = is_budget_too_small(err);
    if !raise && !is_budget_exceeds_max_tokens(err) { return false; }
    if payload.pointer("/thinking/type").and_then(Value::as_str) != Some("enabled") { return false; }
    let Some(max) = payload.get("max_tokens").and_then(Value::as_u64) else { return false };
    let current = payload.pointer("/thinking/budget_tokens").and_then(Value::as_u64);
    let floor = ANTHROPIC_MIN_THINKING_BUDGET;
    if max <= floor {
        // 有历史思考块时不能只关顶层 thinking；本次不改写，交给原失败路径处理。
        if has_thinking(payload) { return false; }
        payload.as_object_mut().unwrap().remove("thinking");
        store.append_event(category, "failover", Some(&key.id), &format!(
            "已关闭本轮扩展思考后重试 · {} · max_tokens={max} 无法容纳合法思考预算；输出上限不变", key.name));
        return true;
    }
    if current == Some(floor) || (raise && current.is_some_and(|n| n > floor))
        || (!raise && current.map_or(true, |n| n < floor)) { return false; }
    payload["thinking"]["budget_tokens"] = Value::from(floor);
    let verb = if raise { "抬高" } else { "降低" };
    store.append_event(category, "failover", Some(&key.id), &format!(
        "已自动{verb}思考预算后重试 · {} · {:?} → {floor}；上游拒绝了预算约束，max_tokens 保持 {max}；不保证重试成功",
        key.name, current));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 三个场景各自对应一类真实上游（判据来自 cc-switch 的实测清单）。
    #[test]
    fn the_three_signature_rejection_shapes_are_recognized() {
        // 场景 1：Anthropic 自己（机器码 + 人类文案两种写法）
        assert!(is_signature_rejection(r#"{"reason":"THINKING_SIGNATURE_INVALID"}"#));
        assert!(is_signature_rejection(
            "content.6: Invalid `signature` in `thinking` block"
        ));
        // 场景 2：Gemini / 第三方
        assert!(is_signature_rejection(
            "Unable to submit request because Thought signature is not valid"
        ));
        // 场景 3：渠道压根不接受这个字段 —— 我们此前完全没覆盖
        assert!(is_signature_rejection(
            "messages.1.content.0.signature: Extra inputs are not permitted"
        ));
        // 不该误伤的：普通鉴权 / 限流 / 只提到 thinking 但与签名无关
        assert!(!is_signature_rejection("invalid api key"));
        assert!(!is_signature_rejection("rate limit exceeded"));
        assert!(!is_signature_rejection(
            "thinking.budget_tokens must be at least 1024"
        ));
    }

    /// 摘掉 thinking / redacted_thinking 块与残留 signature 字段。
    #[test]
    fn thinking_blocks_and_leftover_signatures_are_stripped() {
        let mut p = json!({"messages":[
            {"role":"user","content":[{"type":"text","text":"hi"}]},
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"...","signature":"sigA"},
                {"type":"redacted_thinking","data":"xx"},
                {"type":"text","text":"answer","signature":"strayB"}
            ]}
        ]});
        let r = strip_thinking_blocks(&mut p);
        assert_eq!(r.thinking_blocks, 2, "thinking + redacted_thinking 都要摘");
        assert_eq!(r.signature_fields, 1, "剩下那个块上的残留 signature 也要摘");
        let left = &p["messages"][1]["content"];
        assert_eq!(left.as_array().unwrap().len(), 1);
        assert_eq!(left[0]["type"], json!("text"));
        assert!(left[0].get("signature").is_none());
    }

    /// 🔴 删完会空的消息**整条不动** —— 否则把一个 400 换成另一个更难懂的 400
    ///（Anthropic：assistant 的 content 不能为空）。
    ///
    /// **连它的 `signature` 也不许摘**：那个字段在思考块里是必填的，摘掉换来的是
    /// `…signature: Field required`。第一版的第二个循环无条件摘 signature，
    /// 于是「整条不动」这条边界被同一个函数自己击穿了。
    #[test]
    fn a_message_that_would_become_empty_is_left_alone() {
        let mut p = json!({"messages":[
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"only this","signature":"sigA"}
            ]}
        ]});
        let r = strip_thinking_blocks(&mut p);
        assert_eq!(r.thinking_blocks, 0, "独占 thinking 的消息不许摘");
        assert_eq!(r.signature_fields, 0, "留着的思考块上那个 signature 是必填，不许摘");
        let block = &p["messages"][0]["content"][0];
        assert_eq!(p["messages"][0]["content"].as_array().unwrap().len(), 1, "原样保留");
        assert_eq!(
            block["signature"],
            json!("sigA"),
            "🔴 摘掉它会换来 `signature: Field required`，同样零提示"
        );
    }

    /// 🔴 摘干净了就必须把顶层 `thinking` 一起关掉。
    ///
    /// 只摘块、留着 `thinking:{type:"enabled"}` 换来的是
    /// `Expected \`thinking\` or \`redacted_thinking\`, but found \`text\``——
    /// 那条错误不含 `signature`，既不命中本模块判据也不命中
    /// `upstream::error_hint::annotate`，用户拿到一句零说明的英文。
    #[test]
    fn a_fully_stripped_payload_also_turns_extended_thinking_off() {
        let mut p = json!({
            "thinking": {"type":"enabled","budget_tokens":8192},
            "messages":[{"role":"assistant","content":[
                {"type":"thinking","thinking":"...","signature":"sigA"},
                {"type":"text","text":"answer"}
            ]}]
        });
        let r = strip_thinking_blocks(&mut p);
        assert_eq!(r.thinking_blocks, 1);
        assert!(r.thinking_disabled, "一个思考块都不剩了，顶层 thinking 必须关掉");
        assert!(p.get("thinking").is_none(), "顶层 thinking 应已移除");
    }

    /// 反面：**还剩思考块时不许关** —— 「关着思考却带着思考块」是没取证的组合，
    /// 不拿真实上游试。那种形态本轮刻意不自愈（模块头「已知限制」第 2 条）。
    #[test]
    fn a_surviving_thinking_block_keeps_extended_thinking_on() {
        let mut p = json!({
            "thinking": {"type":"enabled"},
            "messages":[
                // 这条整条不动（思考块独占）
                {"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"s"}]},
                // 这条会被摘，故 out.any() 为真 —— 关键是它**不足以**让我们关掉顶层 thinking
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"y","signature":"s2"},
                    {"type":"text","text":"t"}
                ]}
            ]
        });
        let r = strip_thinking_blocks(&mut p);
        assert_eq!(r.thinking_blocks, 1, "只摘得掉不独占的那一个");
        assert!(!r.thinking_disabled, "还剩一个思考块 → 不许关顶层 thinking");
        assert!(p.get("thinking").is_some());
    }

    /// 幂等：没什么可摘时返回全 0，且一个字节都不改 —— 调用方因此不需要「是否已整流」的标志。
    #[test]
    fn stripping_is_idempotent_and_leaves_clean_payloads_untouched() {
        let clean = json!({"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]});
        let mut p = clean.clone();
        assert_eq!(strip_thinking_blocks(&mut p), Removed::default());
        assert_eq!(p, clean);
        // 非 Anthropic 形态（无 messages）也不该 panic
        let mut other = json!({"input":[{"type":"message"}]});
        assert_eq!(strip_thinking_blocks(&mut other), Removed::default());
    }

    /// 🔴 `is_signature_rejection` 那道门必须真的挡住别的错误。
    ///
    /// 漏掉它的代价不是「少修一次」，而是**每一次上游报错都把整段思考历史剥掉** ——
    /// 429 限流之后本来只要换个 Key 就好，却顺带丢掉了这一轮的思考上下文，
    /// 而用户从日志里只会看到一句「已自动摘除扩展思考块」不知从何而来。
    ///
    /// 这条同时覆盖了「落事件」那一段：`rectify_on_signature_error` 此前零测试覆盖，
    /// 把 `append_event` 整段删掉 5 条用例全绿（那会让「为什么第一条 400、第二条却好了」
    /// 彻底不可见）。
    #[test]
    fn only_signature_rejections_trigger_the_rectifier_and_it_always_leaves_a_trace() {
        let (store, dir) = crate::service::tests::temp_store("thinking_rectify");
        let key = ProviderKey {
            id: "k1".into(),
            name: "测试 Key".into(),
            ..Default::default()
        };
        let with_thinking = json!({
            "thinking": {"type":"enabled"},
            "messages":[{"role":"assistant","content":[
                {"type":"thinking","thinking":"...","signature":"sigA"},
                {"type":"text","text":"answer"}
            ]}]
        });

        // 别的错误：一个字节都不许动，也不许落事件
        let mut p = with_thinking.clone();
        assert!(!rectify_on_signature_error(
            "HTTP 429: rate limit exceeded",
            &mut p,
            &store,
            CategoryType::ClaudeCli,
            &key
        ));
        assert_eq!(p, with_thinking, "非签名错误不许改请求体");
        assert!(
            store.list_all_events().is_empty(),
            "没做任何事就不该落事件，否则日志里会出现无从解释的一行"
        );

        // 签名错误：改了，且必须留痕
        let mut p = with_thinking.clone();
        assert!(rectify_on_signature_error(
            "content.6: Invalid `signature` in `thinking` block",
            &mut p,
            &store,
            CategoryType::ClaudeCli,
            &key
        ));
        let ev = store.list_all_events();
        let hit = ev
            .iter()
            .find(|e| e.detail.contains("已自动摘除扩展思考块"))
            .expect("必须落一条事件 —— 否则用户看到的是「第一条 400、第二条却好了」而无解释");
        assert_eq!(hit.kind, "failover", "该落进「故障转移」组，不是「系统」组");
        assert_eq!(hit.key_id.as_deref(), Some("k1"));
        assert!(hit.detail.contains("降级为不开思考"), "降级这件事必须说出来：{}", hit.detail);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **A10-8 回归：「必须以 thinking 开头」不仅要识别，还要真的自愈。**
    ///
    /// 场景：请求开着 thinking，但最后一条 assistant 消息直接以 `tool_use` 开头，**没有任何**
    /// thinking 块、也没有残留 signature（工具续接的常见形态）。`strip_thinking_blocks` 无块可摘、
    /// 无签名可删（`removed.any()` 本会是 false），若整流就此返回 false，则「识别了却没处理」——
    /// 顶层 `thinking:{enabled}` 原样发回，上游再报同一个 `Expected thinking … found tool_use`。
    /// prefix-error 分支必须关掉顶层 thinking，使 `removed.any()` 为真、整流返回 true。
    #[test]
    fn a_prefix_error_with_no_thinking_block_still_self_heals() {
        let (store, dir) = crate::service::tests::temp_store("prefix_selfheal");
        let key = ProviderKey { id: "k1".into(), name: "测试 Key".into(), ..Default::default() };
        // 开着 thinking，但 assistant 首块是 tool_use、无 thinking 块、无 signature。
        let mut p = json!({
            "thinking": {"type":"enabled","budget_tokens":8192},
            "messages":[{"role":"assistant","content":[
                {"type":"tool_use","id":"c1","name":"grep","input":{}}
            ]}]
        });
        assert!(
            rectify_on_signature_error(
                "messages.69.content.0.type: Expected `thinking` or `redacted_thinking`, but found `tool_use`.",
                &mut p, &store, CategoryType::ClaudeCli, &key,
            ),
            "🔴 A10-8：识别了「必须以 thinking 开头」就必须自愈 —— 没块可摘时改为关顶层 thinking"
        );
        assert!(p.get("thinking").is_none(), "顶层 thinking 必须被关掉，否则上游再报同一个错");
        assert!(
            p["messages"][0]["content"][0]["type"] == "tool_use",
            "只关顶层 thinking，不动消息内容（那条 tool_use 是真实历史）"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **A10-9 回归：max_tokens 窗口不可用时，历史仍有思考块 → 保守拒绝，不只删顶层。**
    ///
    /// `max <= floor` 时预算整流会删顶层 thinking；但若消息历史里还留着 thinking/redacted_thinking
    /// 块，只关顶层会换来「关着思考却带着思考块」这个我们没取证过的组合（与签名整流明确维护的
    /// 边界矛盾）。此时必须 `return false`、交回原失败路径，一个字节都不动。
    #[test]
    fn an_impossible_budget_window_defers_when_thinking_blocks_remain() {
        let (store, dir) = crate::service::tests::temp_store("budget_hasthinking");
        let key = ProviderKey { id: "k1".into(), name: "测试 Key".into(), ..Default::default() };
        let mut p = json!({
            "max_tokens": 1000,   // <= 1024 下限 → 合法窗口不存在
            "thinking": {"type":"enabled","budget_tokens":2048},
            "messages":[{"role":"assistant","content":[
                {"type":"thinking","thinking":"历史思考","signature":"sigA"},
                {"type":"text","text":"answer"}
            ]}]
        });
        let before = p.clone();
        assert!(
            !rectify_on_budget_error(400, "thinking.budget_tokens must be less than max_tokens",
                &mut p, &store, CategoryType::ClaudeCli, &key),
            "🔴 A10-9：历史仍有思考块时不能只删顶层 thinking —— 保守拒绝、交回原失败路径"
        );
        assert_eq!(p, before, "拒绝时一个字节都不许动（含那个顶层 thinking）");
        assert!(store.list_all_events().is_empty(), "没做任何事就不该落事件");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 接线判据：整流必须挂在候选循环的**共享前段**，不许挂进某一条失败分支。
    ///
    /// 上面几条只测本模块 —— 把那一行从 proxy.rs 删掉它们照样全绿，而那就是
    /// 「上游报签名错误、我们照旧原样切下一个候选（同样会 400）」这个缺陷本身。
    /// 这是本仓第 10 次盯同一类接线盲区。
    ///
    /// **而且判据必须钉住位置，不只是「调了」**：失败分支有三条（流式非 2xx /
    /// 非流式非 2xx / 连接层），第一版就只挂在流式那条上 —— 非流式客户端
    /// （`stream:false`）完全得不到自愈，而那是静默的。同 `route_meta` 那条
    /// 「记得在每个出口调一次是必然会漏的纪律」：挂在唯一的共享点才是结构上不可漏。
    #[test]
    fn the_rectifier_must_be_wired_into_the_shared_prologue() {
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        assert_eq!(
            prod.matches("rectify_thinking_signature(&last_err, &mut req_json").count(),
            1,
            "只该有一处调用 —— 就地整流 req_json（它是借给各候选的，改它下一个候选才受益）"
        );
        let at = prod
            .find("rectify_thinking_signature(")
            .expect("失败分支必须就地整流 req_json");
        let stream_branch = prod
            .find("if wants_stream && can_stream(key) {")
            .expect("找不到流式分支 —— 判据失去参照物，先修判据");
        assert!(
            at < stream_branch,
            "整流必须排在三条失败分支**之前**的共享前段；挂进任一分支都会漏掉另两条"
        );
    }

    /// 🔴 补齐的四个签名场景（2026-09-20 对齐 cc-switch 的实测清单）。
    ///
    /// 此前只覆盖场景 1/2/3，而 cc-switch 的清单里还有四条真实上游文案。漏掉它们的
    /// 表现是「同一个成因、换个说法就不自愈了」——用户看到的是随机性。
    #[test]
    fn the_four_newly_covered_signature_shapes_are_recognized() {
        // 场景 4：开着思考时续接的 assistant 消息必须以思考块开头
        assert!(is_signature_rejection(
            "messages.3: final assistant content must start with a thinking block"
        ));
        // 场景 5：同一条约束的另一种说法（要求明确含 tool_use）
        assert!(is_signature_rejection(
            "messages.69.content.0.type: Expected `thinking` or `redacted_thinking`, but found `tool_use`."
        ));
        // 场景 6：签名字段缺失
        assert!(is_signature_rejection(
            "messages.2.content.0.signature: Field required"
        ));
        // 场景 7：思考块被改动过
        assert!(is_signature_rejection(
            "thinking or redacted_thinking blocks from previous turns cannot be modified"
        ));
    }

    /// 🔴 **刻意不抄 cc-switch 的 `invalid request` 宽泛兜底。**
    ///
    /// 它那边任何含 `invalid request` / `非法请求` 的错误都触发整流。抄过来的失效方向是
    /// 「静默改写用户请求」：一条与思考无关的 400（参数名写错、模型名不认）会让我们把
    /// 用户的思考块全摘掉重试 —— 重试照样失败，但日志写着「整流已应用」，
    /// 把排障引向完全错误的方向。有人「顺手对齐」加上它时，这条立刻变红。
    #[test]
    fn the_overly_broad_invalid_request_catch_all_is_deliberately_absent() {
        for unrelated in [
            "invalid request: unknown parameter `foo`",
            "非法请求：模型名不存在",
            "Invalid request - model not found",
        ] {
            assert!(
                !is_signature_rejection(unrelated),
                "与思考无关的错误不许触发签名整流（会静默剥掉用户的思考历史）：{unrelated}"
            );
        }
    }

    /// 🔴 **两条预算判据必须互斥** —— 重叠就是死循环。
    ///
    /// 它们的修复动作方向相反（抬高 / 降低）。判据一旦互相命中，整流会「抬到 1024 → 被拒
    /// → 降到 1024 → 被拒」地来回改同一个字段，而每一轮都花用户一次真实请求。
    /// 这是 `rectify_on_budget_error` 文档里承诺「有判据钉住」的那条。
    #[test]
    fn the_two_budget_predicates_never_both_match() {
        // 「太小」那一类的真实文案（来自 cc-switch 的实测清单）
        let too_small = [
            "thinking.budget_tokens: Input should be greater than or equal to 1024",
            "thinking budget_tokens must be >= 1024",
            "thinking.budget_tokens must be at least 1024",
        ];
        // 「不小于 max_tokens」那一类 —— 2026-09-20 用户实报的原话
        let too_large = [
            "thinking.budget_tokens must be less than max_tokens",
            r#"{"error":{"code":"server_error","message":"thinking.budget_tokens must be less than max_tokens"}}"#,
        ];
        for s in too_small {
            assert!(is_budget_too_small(s), "应判为「太小」：{s}");
            assert!(
                !is_budget_exceeds_max_tokens(s),
                "🔴 「太小」不许同时命中「太大」—— 两个方向相反的修复会来回改同一个字段：{s}"
            );
        }
        for s in too_large {
            assert!(is_budget_exceeds_max_tokens(s), "应判为「不小于 max_tokens」：{s}");
            assert!(
                !is_budget_too_small(s),
                "🔴 「太大」不许同时命中「太小」（同上，死循环）：{s}"
            );
        }
        // 无关错误两条都不许命中
        for s in ["rate limit exceeded", "invalid api key", "model not found"] {
            assert!(!is_budget_too_small(s) && !is_budget_exceeds_max_tokens(s));
        }
        // 🔴 签名判据与预算判据也不许串味：预算错误不该让我们去剥思考块。
        for s in too_large.iter().chain(too_small.iter()) {
            assert!(
                !is_signature_rejection(s),
                "预算问题不该触发签名整流（会白剥掉思考历史）：{s}"
            );
        }
    }

    /// 🔴 用户 2026-09-20 实报那条的自愈：预算降到合法下限后重试。
    ///
    /// 同时钉住**两处刻意不照搬 cc-switch**：`max_tokens` 一个字节都不许动
    /// （它那边会硬写 64000 —— 覆盖用户明确给的输出长度）。
    #[test]
    fn a_budget_that_exceeds_max_tokens_is_lowered_and_max_tokens_is_left_alone() {
        let (store, dir) = crate::service::tests::temp_store("budget_lower");
        let key = ProviderKey {
            id: "k1".into(),
            name: "测试 Key".into(),
            ..Default::default()
        };
        let mut p = json!({
            "max_tokens": 32000,
            "thinking": {"type":"enabled","budget_tokens":32000},
            "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]
        });
        assert!(rectify_on_budget_error(
            400,
            "HTTP 400: thinking.budget_tokens must be less than max_tokens",
            &mut p,
            &store,
            CategoryType::ClaudeCli,
            &key
        ));
        assert_eq!(
            p["thinking"]["budget_tokens"], 1024,
            "预算应降到 Anthropic 合法下限（一次到底，不做多轮试探）"
        );
        assert_eq!(
            p["max_tokens"], 32000,
            "🔴 max_tokens 一个字节都不许动 —— cc-switch 会硬写 64000，那是覆盖用户的输出长度决定"
        );
        let ev = store.list_all_events();
        let hit = ev
            .iter()
            .find(|e| e.detail.contains("已自动降低思考预算"))
            .expect("必须落一条事件 —— 否则用户看到「先 400、重试却好了」而无解释");
        assert_eq!(hit.kind, "failover", "该落进「故障转移」组");
        assert_eq!(hit.key_id.as_deref(), Some("k1"));

        // 幂等 = 终止性：已经在下限上就不再改、不再落事件。
        let before = p.clone();
        let events_before = store.list_all_events().len();
        assert!(
            !rectify_on_budget_error(
                400,
                "HTTP 400: thinking.budget_tokens must be less than max_tokens",
                &mut p,
                &store,
                CategoryType::ClaudeCli,
                &key
            ),
            "🔴 第二次必须是 no-op —— 整流挂在候选循环的共享前段，每个候选失败都会走一次"
        );
        assert_eq!(p, before, "no-op 时一个字节都不该动");
        assert_eq!(store.list_all_events().len(), events_before, "no-op 不该落事件");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 「太小」那一类（cc-switch 会自愈、我们此前完全不自愈）：抬到合法下限。
    #[test]
    fn a_budget_below_the_floor_is_raised() {
        let (store, dir) = crate::service::tests::temp_store("budget_raise");
        let key = ProviderKey { id: "k1".into(), ..Default::default() };
        let mut p = json!({
            "max_tokens": 8192,
            "thinking": {"type":"enabled","budget_tokens":512},
            "messages":[]
        });
        assert!(rectify_on_budget_error(
            400,
            "thinking.budget_tokens: Input should be greater than or equal to 1024",
            &mut p,
            &store,
            CategoryType::ClaudeCli,
            &key
        ));
        assert_eq!(p["thinking"]["budget_tokens"], 1024);
        assert_eq!(p["max_tokens"], 8192, "max_tokens 不许动");
        assert!(
            store
                .list_all_events()
                .iter()
                .any(|e| e.detail.contains("已自动抬高思考预算")),
            "必须留痕"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 **不许替用户开启扩展思考** —— cc-switch 会造一个 `thinking` 对象出来。
    ///
    /// 它有测试 `test_rectify_budget_creates_thinking_object_when_missing` 钉着那个行为：
    /// 一个从没要求思考的请求，整流后开始思考、开始烧思考 token。那是替用户改变意图。
    /// 同理 `type != "enabled"`（含 `adaptive`）时也不动。
    #[test]
    fn the_rectifier_never_switches_extended_thinking_on_for_the_user() {
        let (store, dir) = crate::service::tests::temp_store("budget_nocreate");
        let key = ProviderKey { id: "k1".into(), ..Default::default() };
        for payload in [
            // 压根没有 thinking 字段
            json!({"max_tokens": 8192, "messages":[]}),
            // 有 thinking 但不是 enabled（adaptive 是 Claude 的新形态）
            json!({"max_tokens": 8192, "thinking":{"type":"adaptive"}, "messages":[]}),
            json!({"max_tokens": 8192, "thinking":{"type":"disabled"}, "messages":[]}),
        ] {
            let mut p = payload.clone();
            assert!(
                !rectify_on_budget_error(
                    400,
                    "thinking.budget_tokens must be less than max_tokens",
                    &mut p,
                    &store,
                    CategoryType::ClaudeCli,
                    &key
                ),
                "没开思考的请求不该被整流：{payload}"
            );
            assert_eq!(p, payload, "🔴 一个字节都不许动（cc-switch 会造 thinking 并开启它）");
        }
        assert!(store.list_all_events().is_empty(), "什么都没做就不该落事件");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 两条约束无法同时满足时，降级为「不开思考」，而**不是**去抬 `max_tokens`。
    ///
    /// `max_tokens <= 1024` 时 `budget >= 1024` 与 `budget < max_tokens` 互相矛盾。
    /// cc-switch 的做法是把 `max_tokens` 硬写成 64000；我们关思考 —— 那是
    /// `strip_thinking_blocks` 文档里「三条出路」的第三条，且只作用于这一个请求。
    #[test]
    fn an_impossible_budget_window_disables_thinking_instead_of_raising_max_tokens() {
        let (store, dir) = crate::service::tests::temp_store("budget_impossible");
        let key = ProviderKey { id: "k1".into(), name: "测试 Key".into(), ..Default::default() };
        let mut p = json!({
            "max_tokens": 1000,
            "thinking": {"type":"enabled","budget_tokens":2048},
            "messages":[]
        });
        assert!(rectify_on_budget_error(
            400,
            "thinking.budget_tokens must be less than max_tokens",
            &mut p,
            &store,
            CategoryType::ClaudeCli,
            &key
        ));
        assert!(p.get("thinking").is_none(), "应关掉本轮扩展思考");
        assert_eq!(
            p["max_tokens"], 1000,
            "🔴 绝不许为了凑合法窗口去抬 max_tokens（那是用户明确给的输出长度）"
        );
        let ev = store.list_all_events();
        assert!(
            ev.iter().any(|e| e.detail.contains("已关闭本轮扩展思考")),
            "降级这件事必须说出来：{ev:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 🔴 预算整流的接线判据**不在这里**，在 `proxy.rs` 的
    /// `budget_rectify_is_wired_into_both_forward_paths` + 两条真实链路 e2e
    /// （`codex_budget_400_is_rectified_on_the_{nonstreaming,streaming}_path`）。
    ///
    /// 为什么记在这里:这个模块曾有一条 `the_budget_rectifier_must_be_wired_into_the_shared_prologue_too`,
    /// 它断言整流挂在**候选循环前段**、作用于 `req_json` —— 而 2026-09-21 审查证明那是缺陷本体:
    /// Codex 下游 `req_json` 里没有 `thinking`（是转换链算出来的,落在转发函数内的 `payload` 上）,
    /// 挂那里对 Codex→Anthropic 恒 no-op。整流已下沉到两个转发函数、发送处、作用于转换后的
    /// payload,并按「同 Key 立即重试一次」自愈（单 Key 也能修）。删掉那条错判据、留此说明防回潮。
    #[test]
    fn the_budget_rectifier_is_wired_in_proxy_not_in_the_candidate_prologue() {
        // 与本文件 642 行的签名整流接线判据同一读法（测试 cwd 是 src-tauri）。
        let src = std::fs::read_to_string("src/proxy.rs").unwrap();
        let prod = crate::proxy::custom_headers::production_code_only(&src);
        // 绝不能再出现「作用于 req_json」的旧形态（那是恒 no-op 的错误位置）。
        assert!(
            !prod.contains("rectify_thinking_budget(&last_err, &mut req_json"),
            "🔴 预算整流不许作用于候选循环前段的 req_json（对 Codex 恒 no-op，2026-09-21 缺陷）"
        );
        // 权威判据在 proxy.rs 那三条测试里；此处只钉「它确实被接进 proxy.rs」这个下限。
        assert!(
            prod.contains("rectify_thinking_budget("),
            "预算整流必须接在 proxy.rs 的转发路径里，否则整块是死代码"
        );
    }

    /// 🔴 **A10-4 回归（预算侧）：只有 400/422 才触发整流。**
    ///
    /// 白名单是「误重试」防线：401（鉴权）、429（限流）、5xx（服务器故障）的正文即使恰好带
    /// 「budget_tokens must be less than max_tokens」这类字样，也**不该**被当成请求校验错误去
    /// 降预算重试 —— 那只会白发一次、真问题依旧，还把鉴权/限流误解释成兼容性问题。
    /// 把 `matches!(status, 400 | 422)` 改成 `!is_success` 就会让这条变红。
    #[test]
    fn a_budget_error_body_under_a_non_validation_status_is_ignored() {
        let (store, dir) = crate::service::tests::temp_store("budget_status");
        let key = ProviderKey { id: "k1".into(), ..Default::default() };
        let err = "thinking.budget_tokens must be less than max_tokens";
        for status in [401u16, 403, 429, 500, 502, 503] {
            let mut p = json!({
                "max_tokens": 32000,
                "thinking": {"type":"enabled","budget_tokens":32000},
                "messages":[]
            });
            let before = p.clone();
            assert!(
                !rectify_on_budget_error(status, err, &mut p, &store, CategoryType::ClaudeCli, &key),
                "status={status} 不是请求校验错误，即便正文含预算字样也不该整流"
            );
            assert_eq!(p, before, "status={status} 时一个字节都不许动");
        }
        assert!(store.list_all_events().is_empty(), "什么都没做就不该落事件");
        let _ = std::fs::remove_dir_all(dir);
    }
}
