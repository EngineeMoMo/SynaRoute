//! 给**已知形态**的上游错误附一段可行动说明。
//!
//! # 为什么值得单独一层
//!
//! 上游（尤其第三方中转）回的错误文本，用户看不出「该改什么」。两类最典型：
//!
//! 1. **扩展思考签名校验失败** —— 它长得像密钥/额度问题，而真实成因是「同一段历史换了上游」，
//!    该做的是开新会话或把会话钉在一条 Key 上。
//! 2. **中转站网关自己的转发失败**（`bad_response_status_code`）—— 它长得像我们发错了请求，
//!    而那句话的主语是**中转站**：「我向我的上游转发后收到了 4xx/5xx」。
//!
//! 两类的共同点是：**不给说明，用户一定会去查错的地方**（前者去查密钥和额度，后者来问我们）。
//! 这正是本仓「指错方向的提示比没有提示更糟」那条的正面版本。
//!
//! # 从 `proxy.rs` 搬出来的理由
//!
//! `proxy.rs` 棘轮余量为 0，而这一层注定要随「又见到一种新形态」增长 ——
//! 留在那里等于每加一支都要先腾行。搬走之后 `proxy.rs` 反而降了约 20 行。

/// 命中已知形态时返回一段追加到错误消息末尾的说明；不认识则 `None`。
///
/// **判据一律用机器码优先、人类文案兜底**：中转站会改写 `message` 但很少动 `type`/`reason`。
pub(crate) fn annotate(upstream_err: &str) -> Option<&'static str> {
    if is_thinking_signature(upstream_err) {
        return Some(THINKING_SIGNATURE_NOTE);
    }
    if upstream_err.contains("bad_response_status_code") {
        return Some(GATEWAY_UPSTREAM_NOTE);
    }
    if is_encrypted_reasoning(upstream_err) {
        return Some(ENCRYPTED_REASONING_NOTE);
    }
    None
}

/// Responses 侧「加密的推理内容解不开」。
///
/// # 🔴 这一支是**取证钩子**，不是修复
///
/// 2026-09-06 专门查过一轮，结论是**我们手上没有真实错误文案**：
///
/// - `invalid_encrypted_content`（第三方 README 提到的那个码）在 codex.exe
///   **0.151.0-alpha.7.2 里 0 命中** —— 它若存在也是**上游 API** 的码，不是 Codex 的；
/// - codex.exe 里 `failed to decrypt` 那两处**与推理无关**（一处是 agent identity 的
///   encrypted task id，一处是它自己的 secrets 文件）；
/// - 但机制是确凿的：`reasoning.encrypted_content` 在 codex.exe 里以 Responses API 的
///   `include[]` 取值出现，旁边还有一段官方指引原文「with `store: false` or ZDR,
///   request and replay `reasoning.encrypted_content`」。也就是说 Codex **确实**会
///   请求并回放这个字段，而我们同协议直通时会把它原样转给**另一个账号**。
///
/// 所以本轮**刻意不做字段摘除**（那是 `thinking_rectify` 那条路，而那里「修一个 400 换来
/// 另一个 400」踩过三次，摘字段的边界必须先有真实响应）。这一支做的是**让真实文案到手**：
/// 命中就给一段说明，用户会把它连同上游原文一起报上来 —— 那时才有依据决定要不要整流。
///
/// 判据刻意宽（`encrypted_content` 出现即认），因为这一支只**追加说明**、不改任何请求，
/// 误命中的代价是多一段话；而漏掉的代价是用户拿到一句零说明的英文。
fn is_encrypted_reasoning(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("encrypted_content") || (e.contains("encrypted") && e.contains("reasoning"))
}

/// # 为什么这条也要一段说明
///
/// 与 `THINKING_SIGNATURE_INVALID` **同族**：推理内容由签发它的那个上游账号加密，
/// 换 Key 之后新账号解不开。区别只在协议 —— 那条是 Anthropic 的 `thinking.signature`，
/// 这条是 Responses 的 `reasoning.encrypted_content`。
///
/// 🔴 文案**不许说「已自动处理」**：我们对这一条什么都没做（理由见 [`is_encrypted_reasoning`]）。
/// 反过来要**明确请用户把原文报上来** —— 这是我们拿到真实错误形态的唯一途径。
const ENCRYPTED_REASONING_NOTE: &str =
    "\n\n【SynaRoute 说明】这条与**加密的推理内容**（Responses 协议的 `reasoning.encrypted_content`）有关，\n\
     不是密钥或额度问题。它与 Claude 那边的「扩展思考签名校验失败」是同一族：\n\
     推理内容由**签发它的那个上游账号**加密，续聊时要原样回放给同一个账号；\n\
     故障转移换了 Key、或你把历史会话切到了另一个上游之后，新账号解不开它。\n\
     可行动的两条：\n\
     • **开一个新会话**（历史里没有旧的加密内容就不会再撞）；\n\
     • 把这个会话**固定在一条 Key** 上（分类页里只启用一条，或设为主 Key 并暂停其余）。\n\
     注：SynaRoute 对这个字段**不做任何改写**（与思考块不同，那一条我们会自动摘除后重试）。\n\
     🙏 如果你看到了这段话，请把**上游返回的原始错误文本**一并反馈给我们 ——\
     这类错误的真实文案我们还没拿到，有了它才能判断能不能自动修掉。";

/// 扩展思考签名校验失败的三种写法。
///
/// 机器码 `THINKING_SIGNATURE_INVALID`（最稳）、部分中转站只透传的人类文案、
/// 以及整流没覆盖的后继形态 `Expected …redacted_thinking`（它**不含** `signature` 字样，
/// 漏了这一支就零说明）。
fn is_thinking_signature(err: &str) -> bool {
    err.contains("THINKING_SIGNATURE_INVALID")
        || err.contains("redacted_thinking")
        || (err.contains("thinking") && err.contains("signature"))
}

/// # 为什么这条错误值得一段长说明
///
/// **SynaRoute 从不伪造签名**（响应侧无一处构造 `"type":"thinking"`）。所以这不是转换丢字段，
/// 而是「同一段历史换了上游」这件事本身在 Anthropic 侧不被允许 —— 思考块的签名由**签发它的
/// 那个上游账号**签，故障转移换了 Key 之后新上游验不了旧上游签的名。
///
/// ⚠️ 「我们完全不碰思考块」这句自 2026-08-30 起**不再成立**：命中本错误后
/// [`super::thinking_rectify`] 会把历史里的思考块摘掉、并把顶层 `thinking` 关掉再让后续候选重试。
/// 说明里必须如实写出这一点，否则用户会据「代理不改请求」排除掉真相所在的那个方向。
const THINKING_SIGNATURE_NOTE: &str =
    "\n\n【SynaRoute 说明】这是**扩展思考签名**校验失败，不是密钥或额度问题。\n\
     Claude 的思考块带一个由「签发它的那个上游账号」签的签名，下一轮把历史发回去时上游要验签；\n\
     而故障转移换了 Key 之后，新上游验不了旧上游签的名 —— 于是整段历史被拒。\n\
     可行动的三条：\n\
     • **开一个新会话**（最快：历史里没有旧签名就不会再撞）；\n\
     • 把这个会话**固定在一条 Key** 上（分类页里只启用一条，或把它设为主 Key 且暂时停用其余）；\n\
     • 或在客户端**关掉扩展思考**（没有思考块就没有签名要验）。\n\
     注：SynaRoute 从不伪造签名；本轮已自动摘除思考块并降级为不开思考后重试（日志页「故障转移」组有记录），仍失败才报到这里。";

/// # `bad_response_status_code` 是**中转站网关**在说话，不是上游模型
///
/// 这是 new-api 系面板的错误类型（用户 2026-08-31 的日志里三条 Key 有一条一直回它）。
/// 它的主语是中转站：「我向我的上游转发之后收到了 4xx/5xx」。也就是说故障在
/// **中转站与它的上游之间**，与用户的请求内容、与我们发的密钥都无关。
///
/// 🔴 那个 `request id` 是**中转站自己的**，不是模型厂商的 —— 这一点必须说，否则用户会拿它
/// 去 platform.openai.com / console.anthropic.com 查，而那里根本没有这条记录。
/// 同 `codex.rs` 里占位符那条防线的思路：把人指向**能解决问题的那一方**。
///
/// 换 Key 有没有用取决于「换到的 Key 是否指向另一个中转站」—— 同站的另一把 Key 会撞同一个
/// 网关。这比笼统的「换 Key 也一样」准确：现有文案那句对同站成立、对跨站是错的。
const GATEWAY_UPSTREAM_NOTE: &str =
    "\n\n【SynaRoute 说明】这条 `bad_response_status_code` 是**中转站自己的网关**报的，\n\
     意思是「它向它的上游转发后收到了错误」—— 故障在中转站与模型厂商之间，\n\
     与你的请求内容、也与我们发出的密钥无关（我们这侧连接是通的，否则不会拿到这个响应体）。\n\
     可行动的两条：\n\
     • 那句里的 `request id` 是**中转站自己的**（不是模型厂商的）——\
     拿它找该中转站的客服/工单最直接，模型厂商后台查不到这条记录；\n\
     • 换一条**指向别的中转站**的 Key 试试；同一站点的另一把 Key 会撞上同一个网关，换了也一样。";

#[cfg(test)]
mod tests {
    use super::*;

    /// 三种写法都要认出来 —— 中转站会改写 `message` 但很少动机器码。
    #[test]
    fn all_three_thinking_signature_spellings_are_recognised() {
        for err in [
            r#"{"error":{"type":"invalid_request_error","code":"THINKING_SIGNATURE_INVALID"}}"#,
            "Expected `thinking` or `redacted_thinking`, but found `text`",
            "thinking blocks must have a valid signature",
        ] {
            let note = annotate(err).unwrap_or_else(|| panic!("认不出：{err}"));
            assert!(note.contains("扩展思考签名"), "{err}");
        }
    }

    /// 🔴 `bad_response_status_code` 必须被认出，且说明里要点明「request id 是中转站的」。
    ///
    /// 不认出的代价是实测过的（用户 2026-08-31 的日志）：他拿着这句话来问我们，
    /// 而真正能解决的一方是中转站客服 —— 那个 request id 在模型厂商后台查不到。
    #[test]
    fn a_relay_gateway_forwarding_failure_points_at_the_relay() {
        let err = r#"{"error":{"type":"bad_response_status_code","message":"bad response status code 400 (request id: 202608310746599815437558268d9d6daISC2YZ)"},"type":"error"}"#;
        let note = annotate(err).expect("new-api 的网关错误类型必须被认出来");
        assert!(note.contains("中转站"), "要点明主语是中转站：{note}");
        assert!(note.contains("request id"), "要告诉用户这个 id 该给谁");
        assert!(
            !note.contains("扩展思考"),
            "别把两支搞混 —— 那会把人指向一个完全无关的方向"
        );
    }

    /// 加密推理内容那一支：要认出来、要说清同族关系、**且不许声称我们处理过它**。
    ///
    /// 这一支是取证钩子（理由见 `is_encrypted_reasoning` 的文档）。三条断言各对应一种错法：
    /// 认不出 → 用户拿到一句零说明的英文；说成「已自动处理」→ 用户据此排除掉真相；
    /// 与思考签名那支混淆 → 指向一个我们并没有对它做的自动修复。
    #[test]
    fn encrypted_reasoning_is_recognised_without_claiming_we_fixed_it() {
        for err in [
            r#"{"error":{"message":"Invalid value for 'reasoning.encrypted_content'"}}"#,
            "HTTP 400: could not decrypt encrypted reasoning item",
            r#"{"error":{"code":"invalid_encrypted_content"}}"#,
        ] {
            let note = annotate(err).unwrap_or_else(|| panic!("认不出：{err}"));
            assert!(note.contains("加密的推理内容"), "{err} → {note}");
            assert!(
                note.contains("不做任何改写"),
                "必须如实说我们没动它 —— 否则用户会排除掉真相所在的方向"
            );
            assert!(
                note.contains("原始错误文本"),
                "必须请用户把上游原文报上来：那是我们拿到真实形态的唯一途径"
            );
            assert!(!note.contains("已自动摘除"), "那是思考块那一支做的事，这一支没做");
        }
    }

    /// 两支不许互相串味。
    ///
    /// ⚠️ 判据刻意**不是**「加密那支里不许出现『扩展思考签名』」—— 它的说明里就该点出
    /// 「与那一族同源」，那是有用的上下文（第一版写成不许出现，被自己的文案打回）。
    /// 真正要分清的是**可行动的那一位**：思考块我们会自动摘除后重试，加密内容我们什么都不做。
    #[test]
    fn the_two_families_do_not_cross_annotate() {
        let thinking = annotate("THINKING_SIGNATURE_INVALID").unwrap();
        assert!(thinking.contains("已自动摘除"), "思考块那一支要说清我们做过什么");
        assert!(
            !thinking.contains("encrypted_content"),
            "思考签名那支不该提另一个协议的字段"
        );

        let reasoning = annotate("reasoning.encrypted_content is invalid").unwrap();
        assert!(reasoning.contains("加密的推理内容"));
        assert!(
            !reasoning.contains("已自动摘除"),
            "加密内容我们什么都没做 —— 说做过就是界面撒谎"
        );
    }

    /// 不认识的错误一律不注解：编一段说明比没有说明更糟。
    #[test]
    fn unknown_errors_get_no_note() {
        assert!(annotate("HTTP 502: upstream timeout").is_none());
        assert!(annotate("").is_none());
    }
}
