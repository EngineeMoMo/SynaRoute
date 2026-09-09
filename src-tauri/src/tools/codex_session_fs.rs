//! rollout 文件的低层操作：原子改写首行、mtime 保全、IO 失败归类、路径与 id 推导。
//!
//! 从父模块 [`super`] 搬出来的，直接原因是它生产段顶在 900 行、余量为 0；但这一层本来
//! 就该独立 —— 它是**只认字节与路径**的纯文件操作，父模块那边是「同步/还原」的编排。
//!
//! # 🔴 thread id 必须从**文件名**推导，不能取首行的 `payload.id`
//!
//! fork 出来的子会话文件名带**两个** UUID（`rollout-<时间>-<父id>_<子id>.jsonl`），而它
//! 首行的 `payload.id` 记的是**父**会话的 id（本机 5 个 rollout 逐个核过，2 个是这种形态）。
//! 取 `payload.id` 的后果不是「显示错了」而是**删错东西**：删一个 fork 子会话会执行
//! `DELETE FROM threads WHERE id = <父id>` 并摘掉父会话的 `session_index.jsonl` 行 ——
//! 用户删掉一个派生分支，他真正的那条对话从 Codex 列表里消失了。
//!
//! 判据与 CodexPlusPlus 的 `rollout_thread_id_from_filename` 一致：取 stem 的**末** 36 个
//! 字符并校验 UUID 形态。这也顺带解释了为什么 `threads` 表里只有 3 行而磁盘上有 5 个
//! rollout —— fork 子会话压根没有自己的 `threads` 记录。
//!
//! # mtime 保全
//!
//! 我们只改首行那一个字段，对话正文一个字节不动。不保全 mtime 的表现是：一次接入之后
//! 用户所有历史会话文件的「修改时间」全部跳到现在 —— 备份工具、文件管理器的「最近修改」
//! 排序、任何按 mtime 增量同步的东西都会认为几百个文件刚被改过。CodexPlusPlus 的设计文档
//! 里把这一条列为必须（"Continue to preserve file modification time after rewriting"）。
//!
//! ⚠️ **它让 mtime 不再能当「有没有写过」的判据**（父模块原先那条用例就是这么判的）。
//! 替代判据是**首行字节**：`serde_json` 默认用 `BTreeMap`，重新序列化会把键**按字母排序**，
//! 所以哪怕内容语义相同，一次真实改写也必然改变首行字节。那比 mtime 更难被绕过。

use crate::error::{AppError, AppResult};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 会话目录名。`archived_sessions` 不能漏 —— 漏了的表现是「归档里的旧会话仍 401」，
/// 而用户不会想到「已归档」与「能不能用」有关系。
pub(in crate::tools) const SESSION_DIRS: [&str; 2] = ["sessions", "archived_sessions"];

/// 递归深度上限。Codex 的布局是 `sessions/YYYY/MM/DD/`，3 层足够；给到 6 层是留余量。
///
/// ⚠️ 它防的是**异常深的真实目录树**，不是符号链接环 —— [`walk`] 用的
/// `DirEntry::file_type()` 按 std 的语义**不跟随符号链接**，指向目录的链接
/// `is_dir()` / `is_file()` 双false、直接落进 `_` 被跳过，那种环压根形成不了。
/// （这条原先写的是「防链接环」，代码审查时按 std 文档核出来是错的。）
const MAX_DEPTH: usize = 6;

/// 读文件的第一行，**行尾原样保留** —— 改写时要用它把新首行拼回去，归一成 `\n` 会让整份
/// 文件的行尾与 Codex 自己写的不一致（本仓在行尾上栽过三次，症状都是「看起来改对了、
/// 实际匹配不上」）。用 `read_line` 而不是读全文：首行实测 50 KB 量级
/// （`base_instructions` 内嵌在里面），而 rollout 整体可以到几十 MB。
pub(in crate::tools) fn read_first_line(path: &Path) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(File::open(path)?).read_line(&mut line)?;
    Ok(line)
}

/// 把一行拆成「正文」与「行尾」。
pub(in crate::tools) fn split_eol(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

/// 递归收集 `sessions/` 与 `archived_sessions/` 下的 `rollout-*.jsonl`。
///
/// 目录不存在**不是错误**：干净安装的机器上 `archived_sessions` 常常没有，
/// 而把它当错误会让整次接入失败在一件无关紧要的事上。
pub(in crate::tools) fn collect_rollouts(home: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    for dir in SESSION_DIRS {
        let archived = dir == "archived_sessions";
        walk(&home.join(dir), 0, archived, &mut out);
    }
    out
}

fn walk(dir: &Path, depth: usize, archived: bool, out: &mut Vec<(PathBuf, bool)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, depth + 1, archived, out),
            Ok(t) if t.is_file() => {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    out.push((path, archived));
                }
            }
            _ => {}
        }
    }
}

/// 相对 `$CODEX_HOME` 的路径，统一用 `/` 分隔。`None` = 推导不出相对形态。
///
/// 🔴 **统一分隔符是为了清单能跨平台读**：`strip_prefix` 保留宿主分隔符，Windows 写出的
/// 清单在 macOS 上会被当成一个完整文件名（`file_name()` 在 Unix 上只认 `/`）——
/// `pointer_is_ours` 就是这么在 macOS CI 上连红三个版本的。
///
/// 🔴 **失败必须返回 `None`，不许兜底成绝对路径**：那会同时丢掉「换机器不认领别人的
/// 文件」与 [`resolve_in_home`] 那道遏制（它必然拒绝绝对路径 → 整批会话被算成越界，
/// 而用户看到的解释指向错误方向）。走到这里说明 [`walk`] 的前缀假设被破坏了。
pub(in crate::tools) fn rel_of(home: &Path, path: &Path) -> Option<String> {
    Some(
        path.strip_prefix(home)
            .ok()?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// 把清单里的相对路径解析回绝对路径，并**确认它仍在 `home` 之内**。
///
/// 清单是 `%APPDATA%\SynaRoute\` 下的普通 JSON，被别人手改之后 `rel_path` 可以写成
/// `../..`，而我们拿它去**改写文件**。危害受限（目标首行必须是合法 `session_meta`），
/// 但本仓对同类原语一贯设遏制（`aggregate.rs` 修过两条路径穿透），成本近乎为零。
///
/// **刻意不用 `canonicalize`**：它解析符号链接、且对不存在的路径直接失败，而「文件已被
/// 用户删掉」是还原时的正常情形。三道门的**实际**覆盖面与直觉不符（注入实测）：逐段
/// `Normal` 那道是唯一挡得住 `..` 的（`Path::starts_with` 按 component 比较、不规范化），
/// `is_absolute` 与前缀检查对绝对路径互为冗余 —— 别以为去掉第二道还有东西兜着。
pub(in crate::tools) fn resolve_in_home(home: &Path, rel: &str) -> Option<PathBuf> {
    // 空串必须先挡掉：`Path::new("").components()` 是空迭代器，下面那条 `all()` 对空集
    // 恒真、`home.join("")` 又恰好等于 home 自身，于是它会通过全部三道门，把
    // **`$CODEX_HOME` 目录本身**当成一个 rollout 交出去（写这条判据时当场抓到的）。
    if rel.trim().is_empty() {
        return None;
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return None;
    }
    // 逐段只接受普通名字。`.` 也拒掉：同一个文件两种写法会让「首记即锁」认不出是同一条。
    if !p.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return None;
    }
    let joined = home.join(p);
    joined.starts_with(home).then_some(joined)
}

/// 临时文件的进程内序号。光靠 `pid + 时间戳`不够 —— 本机实测 `timestamp_nanos` 的量化
/// 粒度只有 100ns，同进程并发调用会拿到完全相同的路径，一个的 rename 会顶掉另一个正在写
/// 的临时文件（`ccswitch::db_copy_path` 上踩过：8 线程 16 万采样里 88% 撞名）。
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 进程内自增序号。给需要「唯一后缀」但不走 [`tmp_path_for`] 的调用方用
/// （备份文件名的时间戳段 —— 秒级甚至毫秒级都可能撞）。
pub(in crate::tools) fn seq() -> u64 {
    TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 与目标文件**同目录**的临时文件路径 —— `std::env::temp_dir()` 可能在别的卷上，而
/// `fs::rename` 跨卷会失败，原子性全靠它。
pub(in crate::tools) fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.synaroute-{}-{}.tmp", std::process::id(), seq()))
}

/// 一次改写的副产物：这份 rollout 的正文里有没有 `encrypted_content`。
///
/// 为什么顺手数它：跨账号恢复旧对话时，Responses 的 reasoning 由**签发它的那个账号**加密，
/// 换上游后可能解不开。我们把 provider 指对了，但那条对话仍可能续不下去 —— 用户需要知道
/// 「哪些会话属于这一类」，否则他会把这个已知边界当成我们的 bug 反复排查。
///
/// 在这里数是**零额外 IO**：改写时本来就要把整份文件流过一遍。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::tools) struct RewriteInfo {
    pub had_encrypted_content: bool,
}

const ENCRYPTED_MARK: &[u8] = b"encrypted_content";

/// 一边读一边找 `encrypted_content`。**保留上一块尾部** `PAT.len()-1` 字节再拼着找，
/// 否则标记恰好跨在两次 `read` 的边界上就会漏（那种漏是静默的：报告说「没有加密内容」）。
struct Sniff<R> {
    inner: R,
    tail: Vec<u8>,
    found: bool,
}

impl<R: Read> Read for Sniff<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 && !self.found {
            let mut hay = std::mem::take(&mut self.tail);
            hay.extend_from_slice(&buf[..n]);
            self.found = hay.windows(ENCRYPTED_MARK.len()).any(|w| w == ENCRYPTED_MARK);
            let keep = hay.len().saturating_sub(ENCRYPTED_MARK.len() - 1);
            self.tail = hay[keep..].to_vec();
        }
        Ok(n)
    }
}

/// 用 `next_first` 换掉文件的首行，其余字节**逐字节照搬**。写临时文件后 rename，
/// 于是任何中途失败都不会留下半个文件（这比对几百 MB 的会话做整份备份现实得多）。
///
/// 原文件的 mtime 在 rename 之后**原样恢复**（见模块头）。恢复失败一律忽略 ——
/// 那只是元数据，为它把一次成功的改写判成失败毫无道理。
pub(in crate::tools) fn replace_first_line(
    path: &Path,
    first: &str,
    next_first: &str,
) -> AppResult<RewriteInfo> {
    let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
    let tmp = tmp_path_for(path);
    let mut info = RewriteInfo::default();
    let copy = (|| -> io::Result<()> {
        let mut src = File::open(path)?;
        // 按**字节**偏移跳过首行：`String::len()` 就是 UTF-8 字节数，而 rollout 是 JSONL
        // （必然 UTF-8）。带 BOM 的文件在上一步就因 JSON 解析失败被挡掉了。
        src.seek(SeekFrom::Start(first.len() as u64))?;
        let mut sniff = Sniff { inner: src, tail: Vec::new(), found: false };
        let mut dst = File::create(&tmp)?;
        dst.write_all(next_first.as_bytes())?;
        io::copy(&mut sniff, &mut dst)?;
        info.had_encrypted_content = sniff.found;
        // 先落盘再 rename：崩在这之间留下的是一个孤立的 .tmp（下次扫描不认它，文件名不以
        // rollout- 开头），而不是一个内容不完整的 rollout。
        dst.sync_all()
    })();
    if let Err(e) = copy {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::ToolConfig(format!("改写 {} 失败: {e}", path.display())));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        AppError::ToolConfig(format!("替换 {} 失败: {e}", path.display()))
    })?;
    restore_mtime(path, mtime);
    Ok(info)
}

fn restore_mtime(path: &Path, mtime: Option<std::time::SystemTime>) {
    let Some(mtime) = mtime else { return };
    let Ok(f) = File::options().write(true).open(path) else { return };
    let _ = f.set_times(fs::FileTimes::new().set_modified(mtime));
}

/// 一次 IO 失败到底是「文件被别的进程占着」还是「压根写不进去」。
///
/// 🔴 **这两者的处置完全不同**，而 Windows 把它们都映射成 `PermissionDenied` ——
/// 父模块原先因此只能给一句条件句（「文件被占用，或其所在目录不可写」）。
/// 真正能分开它们的是 **raw OS error**：32 = `ERROR_SHARING_VIOLATION`（别人开着它）、
/// 33 = `ERROR_LOCK_VIOLATION`（区域被锁）。有了它，「退出 Codex 再试」这句指路才是准的
/// —— 在只读卷上说这句话，用户照做之后一个字都不会变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tools) enum IoBlame {
    /// 被别的进程独占。可自助：完全退出 Codex 再来一次。
    Locked,
    /// 权限/只读卷。退出 Codex 没用，要看目录权限。
    Unwritable,
    /// 其它（磁盘满、路径消失…）。原文照报，不猜。
    Other,
}

pub(in crate::tools) fn blame_io(e: &io::Error) -> IoBlame {
    if matches!(e.raw_os_error(), Some(32 | 33)) {
        return IoBlame::Locked;
    }
    match e.kind() {
        // 非 Windows 上没有 32/33 这套码；`WouldBlock` 是 flock 家族的「被占着」。
        io::ErrorKind::WouldBlock => IoBlame::Locked,
        io::ErrorKind::PermissionDenied => IoBlame::Unwritable,
        _ => IoBlame::Other,
    }
}

/// 从错误消息里回收归类结论。
///
/// [`replace_first_line`] 把 `io::Error` 包成了 [`AppError::ToolConfig`] 的字符串（那是既有
/// 形态，改签名要动父模块里 0 余量的编排代码），而调用方只需要「能不能自助」这一位。
/// 判据取 Windows 的 `(os error 32)` 尾注 —— `io::Error` 的 `Display` 一定带它。
///
/// ⚠️ 认不出就返回 [`IoBlame::Other`]，也就是**退回到条件句**。宁可少说一句精确的话，
/// 也不能按字符串猜错方向 —— 指错方向的提示比没有提示更糟。
pub(in crate::tools) fn blame_from_text(msg: &str) -> IoBlame {
    if msg.contains("os error 32") || msg.contains("os error 33") {
        IoBlame::Locked
    } else if msg.contains("os error 5") || msg.contains("os error 13") {
        IoBlame::Unwritable
    } else {
        IoBlame::Other
    }
}

/// 把 Windows 的「扩展长度路径」写成人能读的形态。
///
/// Codex 在 `threads.cwd` / `threads.rollout_path` 里存的是 `\\?\C:\Users\...`（实测本机
/// 3 行全是这个形态），而 rollout 首行里的 `cwd` 是普通 `C:\Users\...`。同一个目录在界面上
/// 出现两种写法，用户会以为是两个不同的位置。UNC 形态（`\\?\UNC\server\share`）要还原成
/// `\\server\share`，直接截掉前缀会得到一个不存在的路径。
///
/// **只用于显示**：路径判据一律走 [`super::resolve_in_home`]，不吃这个函数的输出。
pub(in crate::tools) fn display_path(value: &str) -> String {
    let s = value.trim();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\").or_else(|| s.strip_prefix(r"\\?\unc\")) {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").unwrap_or(s).to_string()
}

/// 从 `rollout-*.jsonl` 的文件名推导 thread id：stem 的**末** 36 个字符必须是 UUID 形态。
///
/// 理由见模块头（fork 子会话的首行 `payload.id` 是**父**会话的 id，拿它去 DELETE 会删错行）。
pub(in crate::tools) fn thread_id_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    // `str::len()` 是字节数，不能直接 `&stem[len-36..]`：带中文/emoji 的手工改名会把
    // 偏移落在 UTF-8 续接字节上并 panic。先按字符找起点，再让 ASCII UUID 判据决定收不收。
    let start = stem.char_indices().rev().nth(35)?.0;
    let tail = &stem[start..];
    let ok = tail.len() == 36 && tail.char_indices().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    ok.then(|| tail.to_string())
}

/// 备份的保留口径。
///
/// 🔴 **两种口径必须分开，不能统一成「留最近 N 份」。** 索引备份是同一个文件反复备份，
/// 数个够了；而删除会话的备份**每条是不同的文件** —— 用户勾 20 条删掉，按数量裁到 5 份
/// 会让另外 15 条的备份当场消失，而确认框里刚写着「会先备份」。那正是本仓最忌的
/// 「界面说做了、实际没做」。
#[derive(Debug, Clone, Copy)]
pub(in crate::tools) enum Keep {
    /// 同一份文件的历史快照，留最近 N 个。
    Count(usize),
    /// 各自独立的文件，按**天数**过期（同日志保留期的口径）。
    Days(u64),
}

/// 把 `src` 拷进 `<data_dir>/backups/<kind>/`，并按 `keep` 裁剪该目录。
///
/// 落在**应用数据目录**（受 `SYNAROUTE_DATA_DIR` 隔离）而不是 Codex 目录旁边：那边多出
/// 一堆 `.bak` 会被用户当成 Codex 自己的东西，而且我们的还原路径也不该去扫它。
///
/// 🔴 **失败必须让调用方中止那次破坏性操作**（返回 `Err` 而不是静默跳过）。备份是我们对
/// 用户的承诺 —— 界面上写了「会先备份」而实际没备份，比压根不提备份糟得多。
///
/// 时间戳带毫秒 + 进程内序号：秒级会让「连点两下」的两次备份同名、后一次覆盖前一次，
/// 于是「保留 N 份」实际只有 1 份（`backup_db` 上真踩过这个）。
pub(in crate::tools) fn backup_file(
    src: &Path,
    data_dir: &Path,
    kind: &str,
    keep: Keep,
) -> Result<PathBuf, String> {
    let dir = data_dir.join("backups").join(kind);
    fs::create_dir_all(&dir).map_err(|e| format!("建备份目录失败: {e}"))?;
    let stem = src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ts = format!("{}-{:04}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"), seq() % 10_000);
    let out = dir.join(format!("{stem}.{ts}.bak"));
    fs::copy(src, &out).map_err(|e| format!("备份 {} 失败: {e}", src.display()))?;
    prune_backups(&dir, keep);
    Ok(out)
}

/// 加了保留就必须同时加清理（同 `log_rotate` 那条），否则备份目录是无界增长。
///
/// 数量口径按**文件名**排序而不是 mtime：时间戳格式定长故字典序即时间序，而备份刚写完
/// 可能同秒 —— 按 mtime 排在那时会随机删掉最新那份。
fn prune_backups(dir: &Path, keep: Keep) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut all: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    match keep {
        Keep::Count(n) => {
            all.sort();
            let cut = all.len().saturating_sub(n);
            for p in &all[..cut] {
                let _ = fs::remove_file(p);
            }
        }
        Keep::Days(days) => {
            let ttl = std::time::Duration::from_secs(days * 24 * 3600);
            let now = std::time::SystemTime::now();
            for p in &all {
                let too_old = fs::metadata(p)
                    .and_then(|m| m.modified())
                    .map(|t| now.duration_since(t).unwrap_or_default() > ttl)
                    .unwrap_or(false);
                if too_old {
                    let _ = fs::remove_file(p);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "synaroute-cxfs-{}-{}-{tag}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// 🔴 fork 子会话的 id 必须取**文件名末尾**那个，不是首行里的父 id。
    ///
    /// 用的是本机真实文件名。判错的后果是删一个派生分支时把**父会话**的 threads 行与索引行
    /// 一起删掉 —— 用户真正的那条对话从 Codex 列表里消失，而他只想删掉一个分支。
    #[test]
    fn a_forked_child_takes_its_own_id_from_the_file_name() {
        let parent = "rollout-2026-09-01T21-06-47-01a05d14-4e5b-7773-b425-25ae029f078f.jsonl";
        let child = "rollout-2026-09-01T21-08-35-01a05d14-4e5b-7773-b425-25ae029f078f_01a05d15-f502-7363-867c-b9ab2203be6f.jsonl";
        assert_eq!(
            thread_id_from_filename(parent).as_deref(),
            Some("01a05d14-4e5b-7773-b425-25ae029f078f")
        );
        assert_eq!(
            thread_id_from_filename(child).as_deref(),
            Some("01a05d15-f502-7363-867c-b9ab2203be6f"),
            "fork 子会话必须拿到**自己**的 id（首行里那个是父会话的）"
        );
        // 认不出的形态一律 None —— 宁可没有 id（跳过 sqlite/索引那两步），也不能猜一个。
        assert_eq!(thread_id_from_filename("rollout-2026-09-01T21-06-47.jsonl"), None);
        assert_eq!(thread_id_from_filename("session_index.jsonl"), None);
        assert_eq!(
            thread_id_from_filename("rollout-xxxx-ZZZZZZZZ-4e5b-7773-b425-25ae029f078f.jsonl"),
            None,
            "非十六进制不算 UUID"
        );
    }

    /// 扩展长度路径只在**显示**层归一，UNC 形态不能直接截前缀。
    #[test]
    fn extended_length_paths_are_normalized_for_display() {
        assert_eq!(
            display_path(r"\\?\C:\Users\Administrator\Desktop\temp\demo3"),
            r"C:\Users\Administrator\Desktop\temp\demo3"
        );
        assert_eq!(display_path(r"\\?\UNC\server\share\p"), r"\\server\share\p");
        // 普通路径原样返回（rollout 首行里的 cwd 就是这个形态）。
        assert_eq!(display_path(r"C:\Users\x"), r"C:\Users\x");
        assert_eq!(display_path("/home/x"), "/home/x");
    }

    /// 改写之后 mtime 必须与改写之前逐纳秒相同，而**首行字节确实变了**。
    ///
    /// 两条断言缺一不可：只断言 mtime 的话，一个「压根没改文件」的实现也能过。
    #[test]
    fn rewriting_the_first_line_preserves_mtime_but_changes_the_bytes() {
        let dir = tmp_dir("mtime");
        let f = dir.join("rollout-a.jsonl");
        fs::write(&f, "{\"type\":\"session_meta\"}\n{\"keep\":1}\n").unwrap();
        // 拨到一个明确的过去时刻，免得「保全」与「刚好同一秒」混在一起。
        let want = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(want))
            .unwrap();

        let info = replace_first_line(&f, "{\"type\":\"session_meta\"}\n", "{\"type\":\"x\"}\n").unwrap();
        assert!(!info.had_encrypted_content);
        let text = fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("{\"type\":\"x\"}\n"), "首行要换掉");
        assert!(text.ends_with("{\"keep\":1}\n"), "其余字节要逐字照搬");
        assert_eq!(
            fs::metadata(&f).unwrap().modified().unwrap(),
            want,
            "mtime 必须保全 —— 否则一次接入会让用户几百个历史会话文件全部显示为刚修改"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// `encrypted_content` 必须被数到，**且跨读块边界也要数到**。
    ///
    /// 判据刻意把标记切在一个大填充的正中间：`io::copy` 的块是 8 KB 量级，不保留上一块尾部
    /// 的实现会在这里静默漏掉它，而漏掉的表现是报告说「没有加密内容」——
    /// 那正好是用户要据此判断「这条旧对话还能不能续」的那一位。
    #[test]
    fn encrypted_content_is_detected_across_read_block_boundaries() {
        let dir = tmp_dir("enc");
        let f = dir.join("rollout-b.jsonl");
        let pad = "x".repeat(8 * 1024 - 8); // 让标记落在第一个 8 KB 块的尾部、被切开
        fs::write(&f, format!("{{\"type\":\"session_meta\"}}\n{pad}encrypted_content{pad}\n"))
            .unwrap();
        let info =
            replace_first_line(&f, "{\"type\":\"session_meta\"}\n", "{\"type\":\"y\"}\n").unwrap();
        assert!(info.had_encrypted_content, "跨块的标记也必须数到");

        // 对照组：没有标记时不许误报。
        let g = dir.join("rollout-c.jsonl");
        fs::write(&g, "{\"type\":\"session_meta\"}\nplain\n").unwrap();
        let info =
            replace_first_line(&g, "{\"type\":\"session_meta\"}\n", "{\"type\":\"y\"}\n").unwrap();
        assert!(!info.had_encrypted_content);
        let _ = fs::remove_dir_all(&dir);
    }

    /// IO 归类：被占用与不可写必须分开，认不出的一律退回 `Other`（= 条件句）。
    #[test]
    fn locked_and_unwritable_are_told_apart() {
        assert_eq!(blame_from_text("替换 x 失败: 拒绝访问。 (os error 32)"), IoBlame::Locked);
        assert_eq!(blame_from_text("... (os error 33)"), IoBlame::Locked);
        assert_eq!(blame_from_text("... (os error 5)"), IoBlame::Unwritable);
        assert_eq!(blame_from_text("... (os error 13)"), IoBlame::Unwritable);
        assert_eq!(blame_from_text("磁盘空间不足"), IoBlame::Other);
        assert_eq!(
            blame_io(&io::Error::from_raw_os_error(32)),
            IoBlame::Locked,
            "ERROR_SHARING_VIOLATION"
        );
        assert_eq!(blame_io(&io::Error::from(io::ErrorKind::PermissionDenied)), IoBlame::Unwritable);
        assert_eq!(blame_io(&io::Error::from(io::ErrorKind::NotFound)), IoBlame::Other);
    }

    /// 🔴 `Keep::Days` 下**不许按数量裁剪**：删除会话的备份每条是不同的文件，
    /// 用户勾 20 条删掉、而我们只留最近 5 份，等于另外 15 条的备份当场消失 ——
    /// 而确认框里刚写着「会先备份」。这是本仓最忌的「界面说做了、实际没做」。
    ///
    /// 判据造 12 个**不同**的源文件（模拟一次删 12 条），断言 12 份备份全在。
    #[test]
    fn per_file_backups_are_never_pruned_by_count() {
        let dir = tmp_dir("bk-days");
        let data = dir.join("appdata");
        for i in 0..12 {
            let f = dir.join(format!("rollout-{i}.jsonl"));
            fs::write(&f, format!("body-{i}")).unwrap();
            backup_file(&f, &data, "many", Keep::Days(30)).unwrap();
        }
        let n = fs::read_dir(data.join("backups").join("many")).unwrap().count();
        assert_eq!(n, 12, "各自独立的文件一份都不许少");
        let _ = fs::remove_dir_all(&dir);
    }

    /// `Keep::Count` 下同一个文件的历史快照裁到 N 份，**且留下的是最新的那些**。
    ///
    /// 排序按文件名（定长时间戳 → 字典序即时间序）而不是 mtime：备份刚写完可能同秒，
    /// 那时按 mtime 排会随机删掉最新那份。
    #[test]
    fn repeated_snapshots_of_one_file_are_pruned_to_the_newest() {
        let dir = tmp_dir("bk-count");
        let data = dir.join("appdata");
        let f = dir.join("session_index.jsonl");
        let mut last = String::new();
        for i in 0..8 {
            fs::write(&f, format!("gen-{i}")).unwrap();
            last = format!("gen-{i}");
            backup_file(&f, &data, "one", Keep::Count(3)).unwrap();
        }
        let kept: Vec<PathBuf> = fs::read_dir(data.join("backups").join("one"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(kept.len(), 3, "同一份文件的历史快照留 3 份");
        assert!(
            kept.iter().any(|p| fs::read_to_string(p).unwrap() == last),
            "留下的必须包含最新那一份，否则裁剪方向反了"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 备份目标目录建不起来时必须返回 `Err`（调用方据此中止那次破坏性操作）。
    ///
    /// 夹具把 `backups` 这个名字用一个**文件**占住 —— 跨平台都成立，不依赖权限位。
    #[test]
    fn a_failed_backup_is_reported_not_swallowed() {
        let dir = tmp_dir("bk-fail");
        let data = dir.join("appdata");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("backups"), b"x").unwrap();
        let f = dir.join("rollout-a.jsonl");
        fs::write(&f, b"body").unwrap();
        assert!(backup_file(&f, &data, "any", Keep::Days(30)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
