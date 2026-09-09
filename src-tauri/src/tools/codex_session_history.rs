//! 顺着 `history_base` 链把 fork 子会话的**完整**历史拼出来。
//!
//! # 它修的缺陷
//!
//! 导出一条 fork 子会话（文件名带两个 UUID）时，导出的对话**缺开头**，而文件上没有任何
//! 迹象表明它被截断了 —— 那是最坏的形态：看起来是一份完整对话。
//!
//! 本机 5 个 rollout 逐个读首行取证（2026-09-09）：fork 的 `session_meta.payload` 里有
//!
//! ```text
//! "history_base": { "thread_id": "01a05d15-…",
//!                   "end_ordinal_exclusive": 53, "end_byte_offset": 149117 }
//! ```
//!
//! 也就是说 **fork 只存分叉点之后的轮次**，之前的在父文件里。实测那条 305 行的 fork 导出后
//! 对话从「优化这个快速排序算法」开始，父会话里的「使用 java 写一个快速排序」不在里面。
//!
//! # 🔴 判据用 `ordinal`，**绝不用 `end_byte_offset`**
//!
//! 两个都指向同一个位置（实测：按字节截到 149117 得到的记录数，与 `ordinal < 53` 的记录数
//! **都是 53**，且偏移正好落在行边界）。但字节偏移在**本仓**是会失效的：
//! [`files::replace_first_line`] 改 provider 时逐字节照搬其余部分，而首行本身长度会变
//! （`serde_json` 用 `BTreeMap`、键会按字母重排，provider 名长短也不同）——
//! 那条测试的名字就叫 `rewriting_the_first_line_preserves_mtime_but_changes_the_bytes`。
//! 于是**我们自己同步一次 provider，父文件里所有字节偏移就整体平移了**，
//! 而按错位的偏移切出来的是「半条 JSON + 后面全部」，解析失败即静默丢记录。
//!
//! `ordinal` 不受影响：改写只动首行**内容**，记录条数与各自的 `ordinal` 一个都不变。
//!
//! ⚠️ **`ordinal` 是全链唯一序号，不是文件内下标**：实测 fork2 的首条 `ordinal` 是 **53**
//! （不是 0），末条是 357，而它只有 305 条。父文件与非 fork 会话才从 0 起。
//! 所以裁剪判据是 `ordinal < end_ordinal_exclusive`，**不能**用「取前 N 行」。
//!
//! # 🔴 `parent_thread_id` 是**另一回事**，刻意不认它
//!
//! 首行里还有一个看起来很像的字段 `parent_thread_id`。**它不是对话分叉**，别照 `history_base`
//! 那样去拼。本机取证（5 个 rollout 逐个读首行）：
//!
//! | thread_source | history_mode | parent_thread_id | history_base |
//! |---|---|---|---|
//! | `user` ×4 | `paginated` | 无 | 1 个有 |
//! | `guardian_review` | `legacy` | **有** | 无 |
//!
//! 也就是它**只出现在 Codex 自己派生的子代理线程上**（`source` 是
//! `{"subagent":{"other":"guardian"}}`）。而那条线程的内容自成一体：正文里是权限裁决
//! （`{"outcome":"deny","rationale":…}`），父会话的历史是**当成一段文本**喂进去的
//! （"The following is the Codex agent history…"），不是结构性继承 —— 没有 ordinal 截断点，
//! 也没有「缺开头」这件事。
//!
//! 拿它去拼会把一条内部审查线程的输出接到用户对话后面，那是**凭空造出一段对话**。
//!
//! # 拼不出来时必须说出来，不能静默
//!
//! 父文件可能已经不在了（用户删过 —— 那正是本应用提供的功能之一）、链深超限、成环，
//! 或者**父文件整份没有 `ordinal`**（legacy 格式，见 [`collect_ancestors`] 末尾那道兜底）。
//! 这时**照旧导出手上这一份**，但在 Markdown 里明写「历史不完整 + 为什么」。
//! 静默截断是这个缺陷的本体，换一种静默截断不算修好。

use super::files;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

/// 链深上限。防的是首行被改坏后互指成环（`a.base → b`、`b.base → a`）——
/// `HashSet` 已经挡住了环，这一条挡的是异常长的合法链，避免一次导出读上百个文件。
const MAX_DEPTH: usize = 32;

/// 拼好的一条会话。
pub(in crate::tools) struct Assembled {
    /// 首行（子会话自己的 `session_meta`）。头部信息一律取**子**的 ——
    /// provider / cwd / 客户端版本这些，用户问的是「我导出的这条对话」，不是它的祖先。
    pub first_line: String,
    /// 全部记录行，**按时间顺序**：祖先的在前，自己的在后。不含上面那条首行。
    pub lines: Vec<String>,
    /// 继承了几个祖先文件。0 = 这条本来就是完整的（非 fork，或 fork 但没有 base）。
    pub inherited: usize,
    /// 拼不完整的原因（Some = 导出要写明这件事）。
    pub incomplete: Option<String>,
}

/// 读一条会话并把它的完整历史拼出来。
///
/// `rel_path` 已由调用方做过路径遏制（`resolve_in_home`）；本函数为祖先**再做一次**
/// —— `history_base.thread_id` 来自文件内容，也就是我们不控制的数据。
pub(in crate::tools) fn assemble(home: &Path, path: &Path) -> Result<Assembled, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读取失败：{e}"))?;
    let mut it = text.lines();
    let first_line = it.next().unwrap_or_default().to_string();
    let own: Vec<String> = it.map(str::to_string).collect();

    let Some(base) = history_base(&first_line) else {
        // 非 fork，或 fork 但没有 base（Codex 早期版本）：手上这份就是全部。
        return Ok(Assembled { first_line, lines: own, inherited: 0, incomplete: None });
    };

    let mut seen = HashSet::new();
    // 把自己的 thread id 先放进去：父指回自己（首行被改坏）时立刻停。
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if let Some(id) = files::thread_id_from_filename(name) {
            seen.insert(id);
        }
    }
    let mut prefix = Vec::new();
    let incomplete = collect_ancestors(home, base, &mut seen, &mut prefix, 0).err();

    let inherited = seen.len().saturating_sub(1);
    let mut lines = prefix;
    lines.extend(own);
    Ok(Assembled { first_line, lines, inherited, incomplete })
}

/// 一条 `history_base` 引用：从哪条 thread 继承、继承到哪个 ordinal（不含）。
#[derive(Clone, Copy)]
struct Base<'a> {
    thread_id: &'a str,
    end_ordinal_exclusive: u64,
}

/// 从首行里取 `history_base`。任何一个字段缺失/类型不对 → None（当成非 fork 处理，
/// 也就是退回本轮改动之前的行为，而不是报错让整次导出失败）。
fn history_base(first_line: &str) -> Option<Base<'_>> {
    // 手动定位而不是 `from_str::<Value>`：返回的 `Base` 借着 `first_line`，
    // 而 Value 是拥有的、活不过本函数。这里只要两个标量。
    let v: Value = serde_json::from_str(first_line).ok()?;
    let b = v.get("payload")?.get("history_base")?;
    let end = b.get("end_ordinal_exclusive")?.as_u64()?;
    let id = b.get("thread_id")?.as_str()?;
    if id.is_empty() {
        return None;
    }
    // 借用期：把 id 在原串里的位置找回来，避免克隆（也让返回的引用挂在 first_line 上）。
    let at = first_line.find(id)?;
    Some(Base { thread_id: &first_line[at..at + id.len()], end_ordinal_exclusive: end })
}

/// 递归收集祖先记录，结果按时间顺序写进 `out`。
///
/// 返回 `Err(原因)` = 拼不完整（父文件找不到 / 读不了 / 链太深 / 成环）。
/// **注意 `out` 在出错时可能已有内容** —— 那是刻意的：能拼多少算多少，
/// 配上一句「不完整，原因是 X」比整段丢掉有用。
fn collect_ancestors(
    home: &Path,
    base: Base<'_>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth >= MAX_DEPTH {
        return Err(format!("继承链超过 {MAX_DEPTH} 层，已停止回溯"));
    }
    if !seen.insert(base.thread_id.to_string()) {
        return Err(format!("继承链出现环（thread {} 重复）", short(base.thread_id)));
    }
    let Some(parent) = find_by_thread_id(home, base.thread_id) else {
        return Err(format!(
            "上一段对话的记录文件已不在（thread {}）—— 它可能已被删除",
            short(base.thread_id)
        ));
    };
    // 🔴 **逐行读 + 提前收手**，不 `read_to_string` 整份：我们只要这个文件**开头**那一段
    // （`ordinal < end_ordinal_exclusive`），而 rollout 可以到几十 MB（`codex_session_fs.rs`
    // 与 `codex_session_view.rs` 都为同一个理由设了上限 / 用流式）。分叉点靠前时，
    // 整份读入等于为了 50 行去加载几十 MB。
    let file = std::fs::File::open(&parent)
        .map_err(|e| format!("读不了上一段对话的记录（thread {}）：{e}", short(base.thread_id)))?;
    let mut reader = std::io::BufReader::new(file);
    let mut first = String::new();
    std::io::BufRead::read_line(&mut reader, &mut first)
        .map_err(|e| format!("读不了上一段对话的首行（thread {}）：{e}", short(base.thread_id)))?;

    // 先递归（祖父在前），再把自己这一段接上 —— 顺序就是时间顺序。
    // 祖父那一段的裁剪由它自己的 base 决定，与本层的 end_ordinal 无关。
    let mut err = None;
    if let Some(up) = history_base(first.trim_end()) {
        err = collect_ancestors(home, up, seen, out, depth + 1).err();
    }

    // 🔴 按 `ordinal` 裁，不按行数 —— ordinal 是全链唯一序号，fork 文件里它不从 0 起。
    //
    // 提前收手的依据是 **ordinal 在文件内严格递增**（本机 5 个文件逐个实测确认）：
    // 一旦看到 `>= end` 就再没有需要的行了。
    let mut kept = 0usize;
    let mut body = 0usize;
    for line in std::io::BufRead::lines(reader).map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        body += 1;
        match ordinal_of(&line) {
            Some(o) if o < base.end_ordinal_exclusive => {
                kept += 1;
                out.push(line);
            }
            Some(_) => break, // 严格递增 → 后面都用不上
            None => {}        // 解析不出 ordinal 的行无法定位，只能丢；下面会兜底判定
        }
    }

    // 🔴 **父文件有正文却一行都没留下 = 静默丢了一整段**，必须报出来。
    //
    // 触发它的真实形态：`history_mode: "legacy"` 的 rollout **整份没有 `ordinal` 字段**
    // （本机那个 `guardian_review` 线程 28 行全缺 —— 我原先在注释里写「实测无一缺失」，
    // 那是只看了 fork 链那 3 个文件就下的结论，**是假话**）。
    // 那时上面的 `match` 每行都走 `None` 分支，祖先段变成空的而 `incomplete` 仍是 None，
    // 也就是本模块要修的缺陷原样复发，只是换了个成因。
    if kept == 0 && body > 0 && err.is_none() {
        return Err(format!(
            "上一段对话的记录里没有可定位的序号（thread {}，{body} 条）—— 它可能是旧格式",
            short(base.thread_id)
        ));
    }
    err.map_or(Ok(()), Err)
}

fn ordinal_of(line: &str) -> Option<u64> {
    serde_json::from_str::<Value>(line).ok()?.get("ordinal")?.as_u64()
}

/// UUID 取前 8 位进用户可见文案 —— 整串 36 字符在一句话里没人读得下去。
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// 按 thread id 找 rollout 文件。判据与列表侧完全一致（[`files::thread_id_from_filename`]
/// 从文件名末 36 字符推导）——**不读首行的 `payload.id`**，那个在 fork 里记的是父的 id。
///
/// 归档目录一起找：分叉点之前的那一段完全可能已经被归档。
fn find_by_thread_id(home: &Path, thread_id: &str) -> Option<std::path::PathBuf> {
    files::collect_rollouts(home).into_iter().find_map(|(path, _archived)| {
        let name = path.file_name()?.to_str()?;
        (files::thread_id_from_filename(name)? == thread_id).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_home(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synaroute-cxhist-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sessions/2026/09/01")).unwrap();
        d
    }

    fn uuid(n: u8) -> String {
        format!("01a05d3b-0017-7ec3-9cff-{:012x}", u64::from(n) + 0x1000)
    }

    /// 造一条 rollout。`base` = Some((父 thread id, end_ordinal))，`ords` = 自己的记录序号。
    /// 返回相对路径。文件名末尾必须是自己的 thread id（`thread_id_from_filename` 的判据）。
    fn write_rollout(
        home: &Path,
        own_id: &str,
        parent_id_in_name: Option<&str>,
        base: Option<(&str, u64)>,
        ords: &[u64],
    ) -> String {
        let stem = match parent_id_in_name {
            Some(p) => format!("rollout-2026-09-01T21-06-47-{p}_{own_id}"),
            None => format!("rollout-2026-09-01T21-06-47-{own_id}"),
        };
        let rel = format!("sessions/2026/09/01/{stem}.jsonl");
        let base_json = base.map_or(String::new(), |(id, end)| {
            format!(
                ",\"history_base\":{{\"thread_id\":\"{id}\",\"end_ordinal_exclusive\":{end},\"end_byte_offset\":999999}}"
            )
        });
        let mut lines = vec![format!(
            "{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{own_id}\",\"timestamp\":\"t\",\"cwd\":\"C:/w\",\"model_provider\":\"synaroute\"{base_json}}}}}"
        )];
        for o in ords {
            lines.push(format!(
                "{{\"ordinal\":{o},\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"轮次{o}\"}}]}}}}"
            ));
        }
        fs::write(home.join(&rel), lines.join("\n") + "\n").unwrap();
        rel
    }

    fn texts(a: &Assembled) -> Vec<u64> {
        a.lines.iter().filter_map(|l| ordinal_of(l)).collect()
    }

    /// 非 fork 会话：拼出来就是它自己，`inherited == 0`、不报不完整。
    #[test]
    fn a_plain_session_is_returned_as_is() {
        let home = tmp_home("plain");
        let rel = write_rollout(&home, &uuid(1), None, None, &[1, 2, 3]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(texts(&a), vec![1, 2, 3]);
        assert_eq!(a.inherited, 0);
        assert!(a.incomplete.is_none(), "{:?}", a.incomplete);
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **缺陷本体**：fork 的历史必须被拼进来，且**按 ordinal 裁**。
    ///
    /// 父有 ordinal 1..=5，fork 从 3 起继承（`end_ordinal_exclusive = 3` → 只要 1、2）。
    /// 若有人改成「取父的前 N 行」，这条会立刻变红：父那 5 条记录 + 首行 = 6 行，
    /// 而 `end_ordinal_exclusive` 是 3。
    #[test]
    fn a_fork_inherits_its_parents_history_up_to_the_ordinal() {
        let home = tmp_home("fork");
        let p = uuid(1);
        let c = uuid(2);
        write_rollout(&home, &p, None, None, &[1, 2, 3, 4, 5]);
        let rel = write_rollout(&home, &c, Some(&p), Some((&p, 3)), &[3, 4]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(texts(&a), vec![1, 2, 3, 4], "父的 1、2（ordinal < 3）+ 自己的 3、4");
        assert_eq!(a.inherited, 1);
        assert!(a.incomplete.is_none(), "{:?}", a.incomplete);
        // 头部信息取**子**的：provider/cwd 问的是被导出的这条。
        assert!(a.first_line.contains(&c), "首行必须是子会话自己的 session_meta");
        let _ = fs::remove_dir_all(&home);
    }

    /// 多层链：孙 → 子 → 父，每层各自按自己的 ordinal 裁，顺序是时间顺序。
    ///
    /// 这一条盯的是「祖父那段的裁剪由它自己的 base 决定」—— 拿本层的 end_ordinal 去裁
    /// 祖父，会把祖父多余/缺失的轮次带进来，而那种错顺序看起来完全正常。
    #[test]
    fn a_chain_of_forks_is_assembled_in_time_order() {
        let home = tmp_home("chain");
        let (a1, b1, c1) = (uuid(1), uuid(2), uuid(3));
        write_rollout(&home, &a1, None, None, &[1, 2, 3, 4, 5, 6]);
        write_rollout(&home, &b1, Some(&a1), Some((&a1, 3)), &[3, 4, 5]);
        let rel = write_rollout(&home, &c1, Some(&b1), Some((&b1, 5)), &[5, 6]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        // 祖父给 1、2（<3）；父给 3、4（<5，且父自己只有 3/4/5）；自己给 5、6。
        assert_eq!(texts(&a), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(a.inherited, 2);
        assert!(a.incomplete.is_none(), "{:?}", a.incomplete);
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 父文件不在了（用户删过）：**照旧导出手上这份，但必须说出来**。
    ///
    /// 静默截断正是本模块要修的缺陷；换一种静默截断不算修好。
    #[test]
    fn a_missing_parent_is_reported_not_silently_dropped() {
        let home = tmp_home("gone");
        let p = uuid(1);
        let c = uuid(2);
        let rel = write_rollout(&home, &c, Some(&p), Some((&p, 3)), &[3, 4]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(texts(&a), vec![3, 4], "自己的轮次照旧要有");
        let why = a.incomplete.expect("父文件不在必须报出来，不能静默返回一段截断的对话");
        assert!(why.contains("已被删除") || why.contains("已不在"), "{why}");
        assert!(why.contains(&p[..8]), "要说清是哪条 thread：{why}");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **父文件整份没有 `ordinal`（legacy 格式）时，不许静默丢掉一整段。**
    ///
    /// ⚠️ 这条是**代码审查补上来的**，而且推翻了我自己写下的一句假话：原注释说
    /// 「Codex 自己写的每一行都有 ordinal（本机 383 行实测无一缺失）」—— 那是只看了 fork 链
    /// 那 3 个文件就下的结论。本机第 4 个文件（`history_mode: "legacy"` 的 `guardian_review`
    /// 线程）**28 行全部没有 `ordinal`**。
    ///
    /// 那时裁剪判据每行都拿不到序号 → 祖先段变成空的 → 而 `incomplete` 仍是 `None`，
    /// 也就是「缺开头且看不出缺了东西」原样复发，只是换了个成因。
    #[test]
    fn a_parent_without_ordinals_is_reported_not_silently_dropped() {
        let home = tmp_home("noord");
        let p = uuid(1);
        let c = uuid(2);
        // 父：legacy 形态，正文有内容但**没有 ordinal 字段**
        let prel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{p}.jsonl");
        fs::write(
            home.join(&prel),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{p}\",\"history_mode\":\"legacy\"}}}}\n{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"旧格式的一轮\"}}]}}}}\n"
            ),
        )
        .unwrap();
        let rel = write_rollout(&home, &c, Some(&p), Some((&p, 99)), &[100]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(texts(&a), vec![100], "自己的轮次照旧要有");
        let why = a
            .incomplete
            .as_deref()
            .expect("父文件一行都没留下，必须报出来而不是静默返回半条对话");
        assert!(why.contains("序号"), "要说清成因是拿不到序号（而不是文件不在）：{why}");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **祖先文件必须逐行读 + 提前收手，不许整份 `read_to_string`。**
    ///
    /// 我们只要它**开头**那一段，而 rollout 可以到几十 MB（`codex_session_fs.rs` 与
    /// `codex_session_view.rs` 都为同一理由用流式 / 设上限）。分叉点靠前时整份读入
    /// 等于为了几十行加载几十 MB。
    ///
    /// 行为侧的判据：在截断点**之后**塞一条巨大的记录，它既不该进结果，也不该被读进内存。
    /// 前者能断言，后者只能靠源码级判据钉住手法 —— 两条一起。
    #[test]
    fn the_ancestor_is_streamed_and_cut_short() {
        let home = tmp_home("stream");
        let p = uuid(1);
        let c = uuid(2);
        let prel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{p}.jsonl");
        let huge = "x".repeat(200_000);
        fs::write(
            home.join(&prel),
            format!(
                "{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{p}\"}}}}\n\
                 {{\"ordinal\":1,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"要保留\"}}]}}}}\n\
                 {{\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{huge}\"}}]}}}}\n"
            ),
        )
        .unwrap();
        let rel = write_rollout(&home, &c, Some(&p), Some((&p, 2)), &[2]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(texts(&a), vec![1, 2], "只该继承 ordinal < 2 的那一条");
        assert!(
            !a.lines.iter().any(|l| l.contains(&huge)),
            "截断点之后那条巨大记录不该出现在结果里"
        );
        assert!(a.incomplete.is_none(), "{:?}", a.incomplete);

        // 源码级：手法必须是流式 + 提前 break，不是整份读入再 filter。
        let me = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_history.rs"
        ));
        assert!(
            me.contains("BufReader::new") && me.contains("BufRead::lines"),
            "祖先必须逐行读（几十 MB 的 rollout 不能整份读入）"
        );
        assert!(
            me.contains("Some(_) => break"),
            "ordinal 在文件内严格递增（本机实测），看到 >= end 必须立刻收手"
        );
        // 反向：本函数里不许对**祖先**用 read_to_string（子会话那一处是必要的，故只许 1 处）。
        assert_eq!(
            me.matches("read_to_string").count(),
            1,
            "只有子会话自己那一份可以整份读；祖先必须流式"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 互指成环不许把栈打爆，且要报出来。
    #[test]
    fn a_cycle_is_broken_and_reported() {
        let home = tmp_home("cycle");
        let (x, y) = (uuid(1), uuid(2));
        // x 的 base 指向 y，y 的 base 指回 x。
        write_rollout(&home, &x, Some(&y), Some((&y, 9)), &[1, 2]);
        let rel = write_rollout(&home, &y, Some(&x), Some((&x, 9)), &[3, 4]);
        let a = assemble(&home, &home.join(&rel)).unwrap();
        let why = a.incomplete.expect("成环必须报出来");
        assert!(why.contains("环"), "{why}");
        let _ = fs::remove_dir_all(&home);
    }

    /// 🔴 **判据必须用 `ordinal`，不许用 `end_byte_offset`。**
    ///
    /// 两者指向同一位置，但字节偏移在**本仓**会失效：`replace_first_line`（改 provider）
    /// 逐字节照搬其余部分而首行长度会变（`serde_json` 的 BTreeMap 会重排键），
    /// 那条测试的名字就叫 `rewriting_the_first_line_preserves_mtime_but_changes_the_bytes`。
    /// 于是同步一次 provider，父文件里所有字节偏移整体平移，按它切出来的是
    /// 「半条 JSON + 后面全部」。
    ///
    /// 本用例把 `end_byte_offset` 写成一个**明显错误**的值（1），若实现改用它，
    /// 拼出来的祖先段会是空的。
    #[test]
    fn the_cut_is_by_ordinal_not_by_byte_offset() {
        let home = tmp_home("byord");
        let p = uuid(1);
        let c = uuid(2);
        write_rollout(&home, &p, None, None, &[1, 2, 3]);
        let rel = format!("sessions/2026/09/01/rollout-2026-09-01T21-06-47-{p}_{c}.jsonl");
        let first = format!(
            "{{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{{\"id\":\"{c}\",\"history_base\":{{\"thread_id\":\"{p}\",\"end_ordinal_exclusive\":3,\"end_byte_offset\":1}}}}}}"
        );
        let body = "{\"ordinal\":3,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"轮次3\"}]}}";
        fs::write(home.join(&rel), format!("{first}\n{body}\n")).unwrap();
        let a = assemble(&home, &home.join(&rel)).unwrap();
        assert_eq!(
            texts(&a),
            vec![1, 2, 3],
            "按 end_byte_offset=1 切会得到空的祖先段 —— 判据必须看 ordinal"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// 拿**真实** `$CODEX_HOME` 跑一遍：夹具是我造的，它只证明「实现符合我理解的格式」。
    ///
    /// 跑法（本机实测 2026-09-09）：
    /// ```text
    /// cargo test --lib assembles_real_rollouts -- --ignored --nocapture
    /// ```
    /// 当时的现场：3 个文件共享一个 session_id —— 父（13 行 / ordinal 0..12）、
    /// fork1（65 行，含父的轮次）、fork2（305 行，ordinal **从 53 起**，
    /// `history_base → fork1 @ 53`）。判据：fork2 拼完必须含父会话最早那句
    /// 「使用 java 写一个快速排序」，而**不拼**的话它从「优化这个快速排序算法」开始。
    ///
    /// 环境里没有会话时**跳过而不是失败**：这条是取证探针，不是所有机器都有现场。
    #[test]
    #[ignore = "要真实 $CODEX_HOME；跑法见文档"]
    fn assembles_real_rollouts() {
        let Ok(home) = crate::tools::codex::codex_paths::codex_home() else {
            eprintln!("跳过：拿不到 CODEX_HOME");
            return;
        };
        let rollouts = files::collect_rollouts(&home);
        if rollouts.is_empty() {
            eprintln!("跳过：{} 下没有 rollout", home.display());
            return;
        }
        let mut forks = 0;
        for (path, _) in &rollouts {
            let a = assemble(&home, path).expect("拼装不该失败");
            let name = path.file_name().unwrap().to_string_lossy();
            let own = std::fs::read_to_string(path).unwrap().lines().count();
            eprintln!(
                "{name}\n  自有 {own} 行 → 拼后 {} 行, 继承 {} 段, 不完整={:?}",
                a.lines.len() + 1,
                a.inherited,
                a.incomplete
            );
            if a.inherited > 0 {
                forks += 1;
                assert!(
                    a.lines.len() + 1 > own,
                    "{name} 继承了 {} 段却没变长 —— 裁剪判据把祖先全滤掉了",
                    a.inherited
                );
                // ordinal 必须单调不减（顺序错了的表现是对话读起来跳来跳去，而它看着正常）
                let ords: Vec<u64> = a.lines.iter().filter_map(|l| ordinal_of(l)).collect();
                assert!(ords.windows(2).all(|w| w[0] <= w[1]), "{name} 拼出来的顺序不是时间序");
            }
        }
        eprintln!("\n共 {} 个 rollout，其中 {forks} 个继承了历史", rollouts.len());
    }

    /// 源码级：本模块生产段不许出现 `end_byte_offset`。
    ///
    /// 上面那条用例守的是行为，这条守的是**手法** —— 有人「顺手优化成按字节 seek」
    /// （看起来更快、还省一次 JSON 解析）时，上面那条会红，但他可能以为夹具写错了。
    /// 这条把理由钉在原地。
    #[test]
    fn the_production_code_must_not_read_the_byte_offset() {
        let me = crate::proxy::custom_headers::production_code_only(include_str!(
            "codex_session_history.rs"
        ));
        assert!(
            !me.contains("end_byte_offset"),
            "字节偏移在本仓会因 replace_first_line 而失效（见模块头与 the_cut_is_by_ordinal…）"
        );
        assert!(
            me.contains("end_ordinal_exclusive"),
            "得真的读 ordinal 那个字段，否则上面那条断言是空洞的"
        );
    }
}
