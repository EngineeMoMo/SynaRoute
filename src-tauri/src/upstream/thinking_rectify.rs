//! 扩展思考签名整流：上游因 `thinking` 块签名验不过而拒绝时，把那些块从请求里摘掉。
//!
//! # 为什么需要它（以及我们原来的定性漏了什么）
//!
//! `upstream::error_hint::annotate` 已经识别这条错误，并把它定性为
//! **「故障转移的固有代价」**：思考块的 `signature` 由签发它的那个上游账号签，
//! 换 Key 之后新上游验不了旧上游的名，于是整段历史被 400 拒。那个成因分析是对的。
//!
//! 但当时只考虑了「换不换 Key」这一个维度，于是给出的三条出路全是让**用户**动手
//! （开新会话 / 固定一条 Key / 关掉扩展思考）。cc-switch 的
//! `src-tauri/src/proxy/thinking_rectifier.rs`（722 行）走的是第三条路：
//! **把验不过的那部分从请求里摘掉再试**。请求会成功，代价只是丢掉那一轮的思考上下文
//! —— 比整个请求失败好得多。
//!
//! 它还揭示了一个我们完全没覆盖的情形（cc-switch 的「场景 5」）：**有些第三方渠道压根
//! 不接受 `signature` 字段**，报 `extra inputs are not permitted`。那不是签名对不上，
//! 是字段本身多余 —— 我们此前对这种情况只会原样报错。
//!
//! # 🔴 为什么是「改请求体让后续候选受益」，而不是「同一个 Key 重打一次」
//!
//! cc-switch 的动作是「对同一供应商重试一次」。我们这里选了不同的落点，理由是
//! `proxy.rs` 的候选循环把 `req_json` 借给每个候选（`Cow::Borrowed`，只在最后一个
//! 候选才 `take`）—— 也就是说**改它一次，后面每个候选自然都用改后的版本**。
//! 这让整流变成**一行接线**，而不用在流式与非流式两条路径里各插一段重试。
//!
//! 🔴 那一行挂在候选循环的**共享前段**（`let next = candidates.get(i + 1);` 之后），
//! 判据是「上一轮的 `last_err` 是签名拒绝就先摘再打」。**不挂进失败分支**：分支有三条
//! （流式非 2xx / 非流式非 2xx / 连接层），第一版只挂了流式那条，于是 `stream:false`
//! 的客户端完全得不到自愈，而那是静默的。同 `route_meta` 那条「记得在每个出口调一次
//! 是必然会漏的纪律」—— 有唯一共享点时就该挂在那里。有测试钉住位置，不只钉「调了」。
//!
//! # 已知限制（两条，都写死在这里免得被当成 bug 重查）
//!
//! 1. **只有一个候选时不会自愈**（没有「下一个」可以受益）。此时用户看到的仍是
//!    那条带三条出路的说明文案。要覆盖单候选就得在转发路径上真加一次重试，而
//!    `proxy.rs` 棘轮余量为 0、且那是转发热路径 —— 留作后续单独一轮。
//! 2. **「思考块独占整条 assistant 消息」这一种形态仍然不自愈**：那种消息我们整条不动
//!    （摘了会 `content` 为空 → 另一个 400），因而顶层 `thinking` 也不敢关
//!    （「关着思考却带着思考块」是没取证的组合）。判据与代价见
//!    [`strip_thinking_blocks`] 的函数文档。现实里带思考的 assistant 消息几乎总还有
//!    `text` 或 `tool_use` 块，故影响面很小。
//!
//! # 三条「修一个 400 换来另一个 400」的坑
//!
//! 都在 [`strip_thinking_blocks`] 的文档里：摘干净后必须关顶层 `thinking`、
//! 留下的思考块不许摘它的 `signature`、剩块时不许关 `thinking`。
//! 这三条错法的共同特征是**换来的那条错误不含 `signature` 字样** ——
//! 既不命中本模块判据、也不命中 `upstream::error_hint::annotate`，
//! 于是用户拿到的是一句零说明的英文，比整流之前更糟。

use crate::model::{CategoryType, ProviderKey};
use crate::store::Store;
use serde_json::Value;

/// 这条上游错误是否属于「签名/思考块不被接受」。
///
/// 三个场景都来自 cc-switch 的实测清单，各自对应一类真实上游：
/// 1. Anthropic 自己：`Invalid 'signature' in 'thinking' block` / `THINKING_SIGNATURE_INVALID`
/// 2. Gemini 及部分第三方：`Thought signature is not valid`
/// 3. 不接受该字段的渠道：`signature … extra inputs are not permitted`
///
/// 判据刻意做成**大小写无关的子串匹配**：这些文案由各家上游自己拼，没有稳定的机器码
/// （只有 Anthropic 给了 `reason`）。脆是脆，但失效方向是**退回现状**（不整流、照旧报错），
/// 不会误伤正常请求。
pub(crate) fn is_signature_rejection(upstream_err: &str) -> bool {
    let e = upstream_err.to_ascii_lowercase();
    // 场景 1：机器码最稳；部分中转站只透传 message，故另按 message 关键词兜一次。
    if e.contains("thinking_signature_invalid")
        || (e.contains("thinking") && e.contains("signature") && e.contains("invalid"))
    {
        return true;
    }
    // 场景 2：Gemini / 第三方的说法
    if e.contains("thought signature") && (e.contains("not valid") || e.contains("invalid")) {
        return true;
    }
    // 场景 3：渠道压根不接受这个字段
    if e.contains("signature") && e.contains("extra inputs are not permitted") {
        return true;
    }
    false
}

/// 整流结果：移除了多少个思考块、多少个残留的 `signature` 字段。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Removed {
    pub thinking_blocks: usize,
    pub signature_fields: usize,
    /// 是否顺带把顶层 `thinking` 也关掉了（= 本轮降级成「不开思考」）。见 [`strip_thinking_blocks`]。
    pub thinking_disabled: bool,
}

impl Removed {
    fn any(&self) -> bool {
        self.thinking_blocks > 0 || self.signature_fields > 0
    }
}

/// 这个块是不是思考块（`thinking` / `redacted_thinking`）。
fn is_thinking(b: &Value) -> bool {
    matches!(
        b.get("type").and_then(Value::as_str),
        Some("thinking") | Some("redacted_thinking")
    )
}

/// 从 Anthropic 形态的请求体里摘掉 thinking / redacted_thinking 块与残留 `signature` 字段。
///
/// **幂等**：没什么可摘时返回全 0 且不改 payload —— 调用方因此不需要维护「是否已整流」的标志。
///
/// # 🔴 摘干净了就必须把顶层 `thinking` 一起关掉
///
/// 只摘块、留着 `thinking: {type:"enabled"}`，换来的是**另一条 400**：开着扩展思考时
/// Anthropic 要求续接工具调用的那条 assistant 消息**以思考块开头**
/// （`Expected \`thinking\` or \`redacted_thinking\`, but found \`text\``）。
/// 而那条错误里没有 `signature` 字样 —— 既不命中本模块的判据、也不命中
/// `upstream::error_hint::annotate`，于是用户拿到的是一句没有任何说明的英文，
/// 比整流之前**更糟**（之前至少有三条出路的提示）。
///
/// 关掉顶层 `thinking` 正是那三条出路里的第三条（「在客户端关掉扩展思考」），
/// 只是由我们代替用户做一次、只作用于这一个请求。
///
/// **但只在「一个思考块都不剩」时才关**：还剩块（下面那条边界保住的那种消息）时，
/// 「thinking 关着却带着思考块」是一个我们没有取证的组合，不拿真实上游试。
/// 那种形态本轮仍然不自愈 —— 已知限制，写在模块头。
///
/// # 边界：只摘「不独占」的 thinking 块，且**不动它们的 `signature`**
///
/// 一条 assistant 消息删掉 thinking 之后如果 `content` 空了，Anthropic 会因
/// 「content 不能为空」再报一次 400。故这种消息整条不动 —— 包括它的 `signature`：
/// 那个字段在思考块里是**必填**的，摘掉只会把一个 400 换成
/// `…signature: Field required`（同样不命中任何判据 → 同样零提示）。
pub(crate) fn strip_thinking_blocks(payload: &mut Value) -> Removed {
    let mut out = Removed::default();
    let mut thinking_left = false;
    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        for msg in messages.iter_mut() {
            let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            let hits = content.iter().filter(|b| is_thinking(b)).count();
            // 见上「边界」：删完会空的消息整条不动，免得把一个 400 换成另一个 400。
            if hits > 0 && hits < content.len() {
                content.retain(|b| !is_thinking(b));
                out.thinking_blocks += hits;
            } else if hits > 0 {
                thinking_left = true;
            }
            // 其余块上残留的 signature 一律摘掉（场景 3：有渠道对这个字段本身报错）。
            // **刻意跳过仍然留着的思考块** —— 它的 signature 是必填字段。
            for block in content.iter_mut().filter(|b| !is_thinking(b)) {
                if let Some(obj) = block.as_object_mut() {
                    if obj.remove("signature").is_some() {
                        out.signature_fields += 1;
                    }
                }
            }
        }
    }
    if out.any() && !thinking_left {
        if let Some(obj) = payload.as_object_mut() {
            out.thinking_disabled = obj.remove("thinking").is_some();
        }
    }
    out
}

/// 失败分支上的一行接线：命中签名拒绝就整流，并落一条可见事件。
///
/// 返回是否真的改了 payload。**不落事件就等于没做** —— 用户看到的现象是
/// 「第一条 Key 400、第二条却好了」，若日志里不说为什么，那是又一个静默行为。
pub(crate) fn rectify_on_signature_error(
    upstream_err: &str,
    payload: &mut Value,
    store: &Store,
    category: CategoryType,
    key: &ProviderKey,
) -> bool {
    if !is_signature_rejection(upstream_err) {
        return false;
    }
    let removed = strip_thinking_blocks(payload);
    if !removed.any() {
        return false;
    }
    store.append_event(
        category,
        // `failover` 而非 `config`：这是「为什么第一条 400、第二条却好了」的唯一解释，
        // 排障的人是在「故障转移」分组里找它的；落进「系统」组等于藏起来。
        "failover",
        Some(&key.id),
        &format!(
            "已自动摘除扩展思考块后重试 · {} · 移除思考块 {} 个、残留签名 {} 处{}\
             （思考块的签名由签发它的那个上游账号签，换 Key 后验不过；摘掉即可继续，\
             代价是丢掉那一轮的思考上下文）",
            key.name,
            removed.thinking_blocks,
            removed.signature_fields,
            if removed.thinking_disabled {
                "、本轮已降级为不开思考"
            } else {
                ""
            },
        ),
    );
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
}
