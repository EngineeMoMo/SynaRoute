//! SSE 六方向黄金样本测试（P3-1.2）。
//!
//! ## 为什么需要这套东西
//!
//! 流式协议转换是与非流式中枢**并行的第二套矩阵**：非流式走 hub-and-spoke（3 协议只需 6 个
//! 单边函数），而流式是 6 个**手写的有向方法**，一千多行、二十多个状态字段。两套矩阵已经
//! 出现过能力漂移（同一个上游信号在这个方向被转过去了、在那个方向被丢了），而在此之前
//! **没有任何一条测试覆盖全部 6 个方向**。
//!
//! 这套测试是 `upstream.rs` 拆分（P2-1）的安全网：拆分本身不该改变任何行为，
//! 而「行为没变」这件事需要一个能自动判定的判据，不能靠人读 diff。
//!
//! ## 只依赖公开面
//!
//! 全文只碰 `SseTranslator` 的 5 个 pub 入口（`new` / `with_namespaces` /
//! `with_namespaces_and_custom` / `push` / `finish`）与 `sse_direction` 一个 pub 函数，
//! 不碰任何私有方法或字段。P2-1 承诺保持 `crate::upstream::X` 的路径不变，
//! 因此这套测试在拆分后应当**零改动**通过 —— 那正是它作为安全网的全部意义。
//!
//! ## 夹具来源等级不同，README 里如实标注
//!
//! `upstream_anthropic.sse` 是本机运行日志里回捞的**真实抓包**；另两份是**按厂商规范构造**的。
//! 手写夹具能守住「重构前后行为一致」，但守不住「我们对上游形态的理解本身就错了」——
//! 这个区别必须留在文档里，不能让后人以为三份都是抓包。

use super::sse::{sse_direction, SseDirection, SseTranslator};
use crate::model::Protocol;

const FIXTURE_ANTHROPIC: &str = include_str!("../testdata/sse/upstream_anthropic.sse");
const FIXTURE_CHAT: &str = include_str!("../testdata/sse/upstream_chat.sse");
const FIXTURE_RESPONSES: &str = include_str!("../testdata/sse/upstream_responses.sse");

/// 上游字节的切分方式。翻译器是行缓冲状态机，其输出**必须与切分方式无关**。
#[derive(Clone, Copy, Debug)]
enum Chunking {
    /// 整份一次喂入
    Whole,
    /// 按 `\n` 切
    PerLine,
    /// 固定窗口。取质数 7：能均匀切在各种字符中间，包括 3 字节中文的任意一半
    Window7,
    /// 逐字节 —— 最狠的一档，能抓出所有跨块状态假设
    Byte,
}

/// 六个方向各自的夹具与名字。
struct Case {
    dir: SseDirection,
    name: &'static str,
    fixture: &'static str,
}

/// 上游形态只有 3 种，每种喂给 2 个下游 → 覆盖全部 6 个方向。
///
/// 同一条上游流被翻两次，这不只是省夹具：**能力漂移会变成一次并排比较**
/// （「推理在这个方向活着、在那个方向没了」），而不是散在两个互不相干的用例里各自看着都合理。
const CASES: &[Case] = &[
    Case { dir: SseDirection::AnthropicToChat, name: "anthropic_to_chat", fixture: FIXTURE_ANTHROPIC },
    Case {
        dir: SseDirection::AnthropicToResponses,
        name: "anthropic_to_responses",
        fixture: FIXTURE_ANTHROPIC,
    },
    Case { dir: SseDirection::ChatToAnthropic, name: "chat_to_anthropic", fixture: FIXTURE_CHAT },
    Case { dir: SseDirection::ChatToResponses, name: "chat_to_responses", fixture: FIXTURE_CHAT },
    Case {
        dir: SseDirection::ResponsesToAnthropic,
        name: "responses_to_anthropic",
        fixture: FIXTURE_RESPONSES,
    },
    Case { dir: SseDirection::ResponsesToChat, name: "responses_to_chat", fixture: FIXTURE_RESPONSES },
];

/// 按指定切分方式回放一份夹具。
fn replay(dir: SseDirection, fixture: &str, chunking: Chunking) -> String {
    let mut tr = SseTranslator::new(dir);
    let bytes = fixture.as_bytes();
    let mut out = String::new();
    match chunking {
        Chunking::Whole => out.push_str(&tr.push(bytes)),
        Chunking::PerLine => {
            for line in fixture.split_inclusive('\n') {
                out.push_str(&tr.push(line.as_bytes()));
            }
        }
        Chunking::Window7 => {
            for c in bytes.chunks(7) {
                out.push_str(&tr.push(c));
            }
        }
        Chunking::Byte => {
            for b in bytes {
                out.push_str(&tr.push(&[*b]));
            }
        }
    }
    out.push_str(&tr.finish());
    out
}

/// 把我方生成的随机 id 换成占位符，让快照可比。
///
/// 判据是「前缀 + 恰好 32 位小写 hex + 紧跟双引号」三个条件**同时**成立。
/// 少任何一个都会出问题：只看前缀会误伤上游 id（夹具里的 `msg_upstream_01` 就该留着）；
/// 不校验长度会匹配到截断的片段；不校验后随引号会在 id 出现在句中时误判。
fn normalize_ids(s: &str) -> String {
    const PREFIXES: &[(&str, &str)] = &[
        ("resp_", "<RESP>"),
        ("msg_", "<MSG>"),
        ("fc_", "<FC>"),
        ("toolu_", "<TOOLU>"),
        ("call_", "<CALL>"),
        ("chatcmpl-", "<CHATCMPL>"),
    ];
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    'outer: while i < bytes.len() {
        for (prefix, placeholder) in PREFIXES {
            if s[i..].starts_with(prefix) {
                let after = i + prefix.len();
                let hex_end = after + 32;
                if hex_end <= bytes.len()
                    && s.is_char_boundary(hex_end)
                    && bytes[after..hex_end]
                        .iter()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
                    && bytes.get(hex_end) == Some(&b'"')
                {
                    out.push_str(placeholder);
                    i = hex_end;
                    continue 'outer;
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 逐行比对并在首个差异处打印上下文。
///
/// 直接 `assert_eq!` 两个几 KB 的字符串，终端里是读不了的 —— 而「读不了」等于
/// 每次回归都要手工 diff 一遍，那这套测试就没人愿意维护了。
fn assert_sse_eq(actual: &str, expected: &str, case: &str) {
    let a: Vec<&str> = actual.lines().collect();
    let e: Vec<&str> = expected.lines().collect();
    for i in 0..a.len().max(e.len()) {
        let (av, ev) = (a.get(i).copied(), e.get(i).copied());
        if av != ev {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(a.len().max(e.len()));
            let mut ctx = String::new();
            for j in lo..hi {
                let mark = if j == i { ">>" } else { "  " };
                ctx.push_str(&format!(
                    "{mark} 行{j}\n{mark}   期望: {}\n{mark}   实际: {}\n",
                    e.get(j).copied().unwrap_or("<无>"),
                    a.get(j).copied().unwrap_or("<无>")
                ));
            }
            panic!(
                "[{case}] 黄金快照不匹配（首个差异在第 {i} 行）\n{ctx}\n\
                 若这是**有意**的行为变更：设 SYNAROUTE_UPDATE_GOLDEN=1 重跑一次以刷新快照，\n\
                 然后逐行核对 diff 并在提交说明里解释每一处的成因。"
            );
        }
    }
}

/// 黄金文件路径。
fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/testdata/sse")
        .join(format!("golden_{name}.sse"))
}

/// 六方向黄金快照。
///
/// 快照本身**不判断对错**，只判断「有没有变」。它的价值全在于：拆 `upstream.rs` 时
/// 任何一处搬错代码都会让某个方向的输出变形，而那个变形会被立刻指出来、精确到行。
#[test]
fn golden_snapshots_match_for_all_six_directions() {
    let update = std::env::var("SYNAROUTE_UPDATE_GOLDEN").is_ok();
    for case in CASES {
        let actual = normalize_ids(&replay(case.dir, case.fixture, Chunking::Whole));
        let path = golden_path(case.name);
        if update {
            std::fs::write(&path, &actual).expect("写黄金文件失败");
            eprintln!("已刷新 {}，请**再跑一遍**确认为绿，并逐行核对 diff", path.display());
            continue;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "读不到黄金文件 {}：{e}\n首次生成请运行：SYNAROUTE_UPDATE_GOLDEN=1 cargo test --lib sse_golden",
                path.display()
            )
        });
        assert_sse_eq(&actual, &expected, case.name);
    }
}

/// **输出必须与字节切分方式无关** —— 本套测试里最抗重构的一条不变性。
///
/// 它不关心事件长什么样，只关心「行缓冲状态机对任意切分等价」，无论 P2-1 怎么搬代码都成立。
///
/// 这条同时是 UTF-8 边界处理的判据：`push` 曾经对每一块入参各自做 `from_utf8_lossy`，
/// 中文被 TCP 分段切开时前后两半各自变成 U+FFFD（用户看到回答里凭空出现「」）。
/// Byte 档在那个修复之前必红。夹具刻意都含中文，就是为了让这条测试有意义。
#[test]
fn output_is_independent_of_chunk_boundaries() {
    for case in CASES {
        let baseline = normalize_ids(&replay(case.dir, case.fixture, Chunking::Whole));
        for chunking in [Chunking::PerLine, Chunking::Window7, Chunking::Byte] {
            let got = normalize_ids(&replay(case.dir, case.fixture, chunking));
            assert_sse_eq(&got, &baseline, &format!("{} / {:?}", case.name, chunking));
        }
        assert!(
            !baseline.contains('\u{FFFD}'),
            "[{}] 输出里出现替换字符 U+FFFD，多字节文本被腐蚀了",
            case.name
        );
    }
}

/// 六个方向一个都不能少（穷举守卫）。
///
/// 将来加第 7 个方向时这条先红，提醒补夹具与快照 —— 而不是让新方向静默地没有任何覆盖。
#[test]
fn every_sse_direction_has_a_golden_case() {
    let all = [
        SseDirection::ChatToResponses,
        SseDirection::ResponsesToChat,
        SseDirection::ChatToAnthropic,
        SseDirection::AnthropicToChat,
        SseDirection::AnthropicToResponses,
        SseDirection::ResponsesToAnthropic,
    ];
    assert_eq!(all.len(), CASES.len(), "方向数与用例数必须一一对应");
    for d in all {
        let n = CASES.iter().filter(|c| c.dir == d).count();
        assert_eq!(n, 1, "{d:?} 应恰好有一个黄金用例，实际 {n} 个");
    }
}

/// `sse_direction` 的映射与 CASES 表一致（防止两处对「谁是上游谁是下游」的理解分叉）。
#[test]
fn sse_direction_mapping_agrees_with_cases() {
    use Protocol::*;
    let expect = [
        (OpenaiResponses, OpenaiChat, SseDirection::ChatToResponses),
        (OpenaiChat, OpenaiResponses, SseDirection::ResponsesToChat),
        (Anthropic, OpenaiChat, SseDirection::ChatToAnthropic),
        (OpenaiChat, Anthropic, SseDirection::AnthropicToChat),
        (OpenaiResponses, Anthropic, SseDirection::AnthropicToResponses),
        (Anthropic, OpenaiResponses, SseDirection::ResponsesToAnthropic),
    ];
    for (down, up, dir) in expect {
        assert_eq!(sse_direction(down, up), Some(dir), "下游 {down:?} × 上游 {up:?}");
    }
    // 同协议不翻译（走直通）
    for p in [Anthropic, OpenaiChat, OpenaiResponses] {
        assert_eq!(sse_direction(p, p), None, "{p:?} 同协议应直通而非翻译");
    }
}

/// 夹具里不得出现「我方生成 id」形态，否则会被规范化规则静默改写。
///
/// 重新抓包时最容易踩：上游真实 id 恰好是 32 位 hex，一进来就被换成 `<MSG>`，
/// 而快照照样是绿的 —— 于是夹具与黄金文件一起悄悄失真。
#[test]
fn fixtures_contain_no_generated_style_ids() {
    for (name, f) in [
        ("anthropic", FIXTURE_ANTHROPIC),
        ("chat", FIXTURE_CHAT),
        ("responses", FIXTURE_RESPONSES),
    ] {
        assert_eq!(
            normalize_ids(f),
            f,
            "[{name}] 夹具里存在会被规范化吞掉的 id（前缀+32hex+引号），请改写成 *_upstream_NN 形态"
        );
    }
}

/// SSE 注释行（`:` 开头）与无 data 的裸 event 行都不得产生任何下游输出。
///
/// 这同时钉住了「夹具首部那几行来源注释不会污染快照」这个前提 —— 那些注释是本套测试
/// 可追溯性的全部依据，必须能安全地留在夹具里。
#[test]
fn comment_and_bare_event_lines_produce_no_output() {
    for case in CASES {
        let mut tr = SseTranslator::new(case.dir);
        let out = tr.push(b": this is a comment\nevent: some_event_without_data\n\n");
        assert!(out.is_empty(), "[{}] 注释行/裸 event 行不该产出内容：{out}", case.name);
    }
}

/// 一条流里能观察到的下游信号。
#[derive(Debug, PartialEq, Eq)]
struct Signals {
    text: bool,
    reasoning: bool,
    tool: bool,
    usage_in: bool,
    usage_out: bool,
    terminal: bool,
}

/// 按**下游协议的表面事件**判定信号。
///
/// 刻意只看下游看得见的东西：这张表要回答的问题是「客户端最终能不能拿到这个能力」，
/// 而不是「翻译器内部有没有处理过」。
fn probe(down: Protocol, raw: &str) -> Signals {
    let has = |pat: &str| raw.contains(pat);
    match down {
        Protocol::OpenaiChat => Signals {
            text: has(r#""content":""#) && !has(r#""content":"""#),
            reasoning: has("reasoning_content"),
            tool: has("tool_calls"),
            usage_in: has("prompt_tokens"),
            usage_out: has("completion_tokens"),
            terminal: raw.trim_end().ends_with("[DONE]"),
        },
        Protocol::Anthropic => Signals {
            text: has("text_delta"),
            reasoning: has("thinking_delta"),
            tool: has(r#""type":"tool_use""#),
            // Anthropic 把输入 token 放在 message_start、输出放在 message_delta
            usage_in: raw
                .lines()
                .any(|l| l.contains("message_start") && l.contains(r#""input_tokens""#) && !l.contains(r#""input_tokens":0"#)),
            usage_out: raw
                .lines()
                .any(|l| l.contains("message_delta") && l.contains("output_tokens")),
            terminal: has("message_stop"),
        },
        Protocol::OpenaiResponses => Signals {
            text: has("response.output_text.delta"),
            reasoning: has("response.reasoning_summary_text.delta"),
            tool: has(r#""type":"function_call""#)
                || has(r#""type":"custom_tool_call""#)
                || has(r#""type":"tool_search_call""#),
            usage_in: has("input_tokens"),
            usage_out: has("output_tokens"),
            terminal: has("response.completed"),
        },
    }
}

/// **能力矩阵** —— 本套测试的核心资产。
///
/// 六个方向各自把「文本 / 推理 / 工具 / 输入用量 / 输出用量 / 终止事件」转过去了没有，
/// 逐格钉死。快照测的是「输出有没有变形」，这张表测的是「能力有没有悄悄消失」——
/// 后者才是流式这套手写矩阵最容易出的问题，而且**不会有任何报错**：
/// 用户只会觉得「换了个中转商之后思考过程就看不见了」，然后归咎于模型。
///
/// 规矩：**每个 false 后面都必须写清是「有意为之」还是「已知缺口」**，不许留空格。
/// 空着的格子等于没钉 —— 后人无从判断改动是修复还是回归。
#[test]
fn capability_matrix_is_pinned() {
    struct Row {
        name: &'static str,
        down: Protocol,
        expect: Signals,
    }
    let rows = [
        Row {
            name: "anthropic_to_chat",
            down: Protocol::OpenaiChat,
            expect: Signals {
                text: true,
                // false = **夹具所限**，非缺口：这份真抓包没开 thinking，流里本就没有推理内容。
                // 想把这格变成有效判据，需要一份带 thinking 的 Anthropic 抓包。
                reasoning: false,
                // 同上：这份抓包是纯对话，没有工具调用。
                tool: false,
                usage_in: true,
                usage_out: true,
                terminal: true,
            },
        },
        Row {
            name: "anthropic_to_responses",
            down: Protocol::OpenaiResponses,
            expect: Signals {
                text: true,
                reasoning: false, // 同上，夹具所限
                tool: false,      // 同上，夹具所限
                usage_in: true,
                usage_out: true,
                terminal: true,
            },
        },
        Row {
            name: "chat_to_anthropic",
            down: Protocol::Anthropic,
            expect: Signals {
                text: true,
                // false = **已知能力缺口**。上游 Chat 流里确实有 reasoning_content（夹具带两片），
                // 但 chat→anthropic 方向没有把它转成 thinking 块，推理内容对下游彻底消失。
                // 用户感受：接 DeepSeek/GLM 思考档时「模型好像不思考了」。
                reasoning: false,
                tool: true,
                // false = **已知能力缺口**。Chat 的 usage 在流尾才到，而 Anthropic 语义要求
                // input_tokens 出现在 message_start（那时还不知道），于是只能填 0；
                // 到流尾拿到 prompt_tokens=128 时又没有回填进 message_delta。
                // 后果：下游按 Anthropic 记账时输入用量恒为 0，成本统计偏低。
                usage_in: false,
                usage_out: true,
                terminal: true,
            },
        },
        Row {
            name: "chat_to_responses",
            down: Protocol::OpenaiResponses,
            expect: Signals {
                text: true,
                // false = **已知能力缺口**，与 chat_to_anthropic 同源：reasoning_content
                // 没有转成 response.reasoning_summary_text.delta。
                reasoning: false,
                tool: true,
                usage_in: true,
                usage_out: true,
                terminal: true,
            },
        },
        Row {
            name: "responses_to_anthropic",
            down: Protocol::Anthropic,
            expect: Signals {
                text: true,
                // false = **已知能力缺口**。上游 Responses 有 reasoning_summary_text.delta，
                // 但没转成 thinking_delta。三个「转出推理」的方向全部缺失，是同一类问题。
                reasoning: false,
                tool: true,
                usage_in: false, // 与 chat_to_anthropic 同源：Responses 的 usage 也在流尾
                usage_out: true,
                terminal: true,
            },
        },
        Row {
            name: "responses_to_chat",
            down: Protocol::OpenaiChat,
            expect: Signals {
                text: true,
                reasoning: false, // 已知能力缺口，同上
                tool: true,
                usage_in: true,
                usage_out: true,
                terminal: true,
            },
        },
    ];

    assert_eq!(rows.len(), CASES.len(), "矩阵行数必须与方向数一致");
    for row in &rows {
        let case = CASES.iter().find(|c| c.name == row.name).expect("矩阵行名必须对应一个用例");
        let out = replay(case.dir, case.fixture, Chunking::Whole);
        let got = probe(row.down, &out);
        assert_eq!(
            got, row.expect,
            "[{}] 能力矩阵变了。\n若这是**修复**（false→true）：更新本表并删掉对应的「已知缺口」注释。\n\
             若这是**回归**（true→false）：说明某次改动把一项能力弄丢了，不要改表，去修代码。\n实际输出：\n{out}",
            row.name
        );
    }
}

