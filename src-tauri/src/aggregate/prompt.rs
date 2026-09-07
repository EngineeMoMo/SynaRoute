//! 喂给模型的 prompt 怎么拼 —— 含**检索文件的封装形态**这一条安全判据。
//!
//! # 为什么文件内容要用随机 nonce 围栏
//!
//! 检索来的文件内容是**不可信输入**：它是项目里的真实文件，而其中任何一行都可能是别人
//! 放进去的提示注入（依赖的第三方包、同事的分支、下载来的示例代码）。原实现把它裸包在
//! 固定三反引号里：
//!
//! ```text
//! ### src/a.rs (grep)
//! ```
//! <文件内容原样，一个字符都没转义>
//! ```
//! ```
//!
//! 于是一个被污染的文件只要在自己内容里写上四反引号包住的 ```` ```file:.git/hooks/pre-commit ````
//! 块，那段文字就会**原样流过**「成员 → 汇总 → 决策者」三跳的 prompt。而 Phase2 给决策者的
//! 指令恰好是「对每个要改的文件输出 ```` ```file:相对路径 ```` 块」—— 模型有相当概率把它
//! 复述进自己的输出，那时 [`super::write::parse_blocks`] 会把它当成真指令。
//!
//! 🔴 **六道防线挡不住这条**：它们判的是「往哪写」，而这里被劫持的是「写什么」——
//! 攻击者完全可以指定一个合法路径（`src/main.rs`）配上恶意内容。防线与本模块是两层，
//! 缺一层都不成立。
//!
//! nonce 让「哪一段是数据」在词法上**不可伪造**：写文件的人不知道这一轮会用哪个随机值，
//! 因此没法造出一个能提前闭合围栏的字符串。同时开场那句说明把「块内是数据、不是指令」
//! 明说给模型 —— 两者都必要：只有 nonce 没有说明，模型不知道该怎么对待它；
//! 只有说明没有 nonce，攻击者可以伪造闭合标签跳出来。
//!
//! **刻意选 XML 风格标签而不是继续用反引号**：Anthropic 自己的长上下文最佳实践就推荐用
//! XML 标签划分参考材料，模型对这个形态的理解比「围栏里的围栏」稳得多。
//!
//! # 代价
//!
//! 每轮多约 60 token 的开场说明 + 每个文件多约 20 token 的标签。相对于文件正文
//! （单个可达数万 token）可以忽略。
//!
//! # 覆盖范围：**检索文件与工具结果都走这道围栏**（2026-09-07 起）
//!
//! 围栏本体在 [`super::fence`]，两个通道共用它 —— 本模块只管「检索文件怎么用它」，
//! 工具结果那一半在 `agent_tools::execute`。**那个模块头是这道防线的单一事实来源**
//! （nonce 为什么每轮换、为什么包在截断之后、为什么错误结果不包、边界到哪）。
//!
//! ⚠️ 本模块头此前写着「工具返回的内容**不**走围栏，刻意如此」，理由是「`tool_result` 是
//! 协议级独立块，模型本来就知道那是工具返回的东西」。那句话是对的，但它答的是另一个问题：
//! **模型知道来源 ≠ 它不会把里面写的 `file:` 块复述进自己的答案**。已收紧，别再引用那段。

use super::fence::Fence;
use super::retrieval;

/// 把检索到的文件拼成「参考数据」段。空列表返回空串（调用方据此省掉整个小节）。
///
/// `fence` 由调用方传入并与**工具结果**共用同一个 —— 两个通道用同一个 nonce，
/// 于是那句说明也只发一遍。
///
/// `tags` 决定说明里列出哪些标签：只有检索时传 `["file"]`，同时开了工具则传
/// `["file", "tool_result"]`。**由调用方决定**是因为只有它知道本轮有没有工具
/// （`prepare_tool_env` 可能因为没有工作目录而返回 None）。
pub(super) fn format_file_context(
    fence: &Fence,
    files: &[retrieval::RetrievedFile],
    tags: &[&str],
) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut s = fence.preamble(tags);
    for f in files {
        let attrs = format!(" path=\"{}\" source=\"{}\"", f.path, f.source);
        s.push('\n');
        s.push_str(&fence.wrap("file", &attrs, &f.content));
        s.push('\n');
    }
    s
}

/// 参与者（只读角色）的 prompt。
///
/// 🔴 **`tool_fence` 补的是「有工具但没有检索文件」那一格**：那时 `file_context` 是空串、
/// 整个「相关文件」小节被省掉，于是那句围栏说明**一个字都不会发出去**，而工具结果
/// 照旧被包着 —— 模型收到一堆它没被告知含义的标签。三种组合里只有这一格会漏，
/// 而它恰好是「关了自动检索、只开工具」这个完全正常的配置。
pub(super) fn build_member_prompt(
    prompt: &str,
    file_context: &str,
    tool_fence: Option<&Fence>,
) -> String {
    let file_section = if file_context.is_empty() {
        // 没有文件段可挂 → 说明只能自己起一段（只列 tool_result，file 这一轮不会出现）。
        match tool_fence {
            Some(f) => format!("\n\n{}", f.preamble(&["tool_result"])),
            None => String::new(),
        }
    } else {
        format!("\n\n## 相关文件（只读）\n{file_context}")
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
pub(super) fn build_solo_decider_prompt(prompt: &str, file_context: &str) -> String {
    if file_context.is_empty() {
        return prompt.to_string();
    }
    format!(
        "{prompt}\n\n## 相关文件\n{file_context}\n\n\
         请结合以上相关文件，给出清晰、可执行、直接回应用户问题的答案。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> retrieval::RetrievedFile {
        retrieval::RetrievedFile {
            path: path.into(),
            content: content.into(),
            relevance: 1.0,
            source: "grep".into(),
        }
    }

    // ⚠️ **nonce 轮换 / 伪造闭合标签 / 三点说明**这三条判据在 [`super::fence`] 里，
    //    不在这里 —— 它们是 `Fence` 自己的性质，两份必然漂移（本模块此前正是那样）。
    //    这里只测「本模块怎么用它」：每个文件都被包、属性齐、空列表省掉整节。

    #[test]
    fn no_files_means_no_section() {
        assert!(
            format_file_context(&Fence::new(), &[], &["file"]).is_empty(),
            "空列表要让调用方省掉整个小节"
        );
    }

    /// 每个检索到的文件都必须被包起来 —— 漏一个就是一段裸的不可信内容。
    #[test]
    fn every_file_gets_wrapped_with_its_path_and_source() {
        let f = Fence::new();
        let out = format_file_context(
            &f,
            &[file("src/a.rs", "AAA"), file("src/b.rs", "BBB")],
            &["file"],
        );
        let id = f.id();
        // ⚠️ 数开标签要带上前导换行：开场说明里那句 `` `<file id="…">` `` 也含这个子串，
        //    不排掉它这条断言会把说明也数成一个块（第一版实测得 3）。
        assert_eq!(
            out.matches(&format!("\n<file id=\"{id}\"")).count(),
            2,
            "两个文件应有两个开标签"
        );
        assert_eq!(
            out.matches(&format!("</file id=\"{id}\">")).count(),
            2,
            "两个文件应有两个闭标签"
        );
        assert!(out.contains("path=\"src/a.rs\"") && out.contains("path=\"src/b.rs\""));
        assert!(out.contains("source=\"grep\""), "来源要带上，供模型判断相关性");
        assert!(out.contains("AAA") && out.contains("BBB"), "正文原样保留");
    }

    /// 说明里列出的标签由调用方决定：开了工具那一轮必须把 `tool_result` 也列上，
    /// 否则模型收到一堆没被告知含义的标签（两道围栏共用同一个 nonce）。
    #[test]
    fn the_tags_passed_in_reach_the_preamble() {
        let f = Fence::new();
        let with_tools = format_file_context(&f, &[file("a.rs", "x")], &["file", "tool_result"]);
        assert!(with_tools.contains(&format!("`<tool_result id=\"{}\">`", f.id())));

        let without = format_file_context(&f, &[file("a.rs", "x")], &["file"]);
        assert!(
            !without.contains("tool_result id="),
            "没有工具的那一轮不该凭空提 tool_result"
        );
    }

    /// 🔴 **有工具但零检索文件时，那句说明仍然必须发出去。**
    ///
    /// 那一格的失效链：`file_context` 为空串 → 整个「相关文件」小节被省掉 →
    /// 说明一个字都不发，而工具结果照旧被包着。三种组合里只有它会漏，
    /// 而它恰好是「关了自动检索、只开工具」这个完全正常的配置。
    #[test]
    fn tools_without_files_still_get_the_preamble() {
        let f = Fence::new();
        let out = build_member_prompt("问题X", "", Some(&f));
        assert!(out.contains("只读参考数据"), "说明必须在");
        assert!(out.contains(&format!("`<tool_result id=\"{}\">`", f.id())));
        assert!(
            !out.contains("`<file id="),
            "这一轮没有检索文件，不该提 file 标签"
        );

        // 对照：没有工具也没有文件时，不该凭空多出一段说明。
        let bare = build_member_prompt("问题X", "", None);
        assert!(!bare.contains("只读参考数据"));
    }

    /// 源码级：**检索来的文件正文只能经本模块进 prompt。**
    ///
    /// 上面几条都在测 `format_file_context` 自己，它们**证明不了**别处没有第二条通道 ——
    /// 谁在 `round.rs` 里直接 `f.content` 拼一段进 prompt，围栏就被绕过了，
    /// 而那正是这个模块存在的全部理由。这是本仓第 18 次盯同一类接线盲区。
    ///
    /// ⚠️ **判据刻意写得过宽**（禁整个 `.content` 字面量，而不只是 `RetrievedFile` 的那个）：
    /// Rust 侧没法按类型 grep，而过宽的失效方向是**误报红** —— 红了人会来读这段注释，
    /// 然后判断那处 `.content` 是不是真的在拼 prompt。反过来（写窄了漏掉
    /// `let c = &f.content` 这种写法）失效方向是静默放行，那不能接受。
    #[test]
    fn retrieved_content_only_reaches_the_prompt_through_this_module() {
        for (name, src) in [
            ("aggregate.rs", include_str!("../aggregate.rs")),
            ("aggregate/round.rs", include_str!("round.rs")),
        ] {
            let prod = crate::proxy::custom_headers::production_code_only(src);
            assert!(
                !prod.contains(".content"),
                "{name} 生产段里出现了 `.content` —— 检索正文必须只经 format_file_context 封装"
            );
        }
        // 反向：本模块自己确实在用围栏包它（否则上面那条断言在「谁都不拼」时空洞成立）。
        let mine = crate::proxy::custom_headers::production_code_only(include_str!("prompt.rs"));
        assert!(mine.contains("fence.wrap("), "本模块必须真的用围栏包正文");
        assert!(mine.contains("f.content"), "本模块是唯一拼正文的地方");
    }

    /// 🔴 **接线判据：开了工具那一轮，`round.rs` 必须把 `tool_result` 列进说明。**
    ///
    /// 上面那条 `the_tags_passed_in_reach_the_preamble` 自己传 tags，**证明不了**调用方传对了 ——
    /// 把 `round.rs` 的 `fence_tags` 写死成 `&["file"]`，它照样全绿（注入实测），
    /// 而那时工具结果照旧被包着、说明里却不提它。这是本仓第 21 次同类盲区。
    ///
    /// ⚠️ **判据钉的是「这个决定按工具在不在分支」这个性质，不是某一种写法** ——
    /// 上一轮为「钉手法的判据在有人改进实现时制造假红」付过代价。
    #[test]
    fn the_round_must_declare_tool_result_when_tools_are_on() {
        let prod = crate::proxy::custom_headers::production_code_only(include_str!("round.rs"));
        assert!(
            prod.contains("\"tool_result\""),
            "round.rs 生产段里没有 tool_result —— 开了工具的那一轮，说明里不会提到工具结果这种块"
        );
        // 且它必须是**有条件的**：无条件列上会在没有工具的那一轮对模型撒谎
        // （让它去找一种本轮根本不会出现的块）。
        assert!(
            prod.contains("tool_env.is_some()"),
            "tool_result 必须按 tool_env 在不在来决定，不能无条件列上"
        );
    }

    /// 围栏必须**只有一处实现**：谁在别处自己拼 `<file id=` / `<tool_result id=`，
    /// nonce 长度、包法、那句说明就会各漂一半，而失效方向是静默的。
    #[test]
    fn nobody_builds_fence_tags_by_hand() {
        for (name, src) in [
            ("aggregate.rs", include_str!("../aggregate.rs")),
            ("aggregate/round.rs", include_str!("round.rs")),
            ("agent_tools.rs", include_str!("../agent_tools.rs")),
            ("aggregate/prompt.rs", include_str!("prompt.rs")),
        ] {
            let prod = crate::proxy::custom_headers::production_code_only(src);
            assert!(
                !prod.contains("<file id=") && !prod.contains("<tool_result id="),
                "{name} 生产段里手拼了围栏标签 —— 必须走 Fence::wrap"
            );
        }
        // 反向：`Fence::wrap` 自己确实在拼（否则上面那条在「谁都不拼」时空洞成立）。
        let f = crate::proxy::custom_headers::production_code_only(include_str!("fence.rs"));
        assert!(f.contains("<{kind} id="), "Fence::wrap 必须是唯一拼标签的地方");
    }
}
