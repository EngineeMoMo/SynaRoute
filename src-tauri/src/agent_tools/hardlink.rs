//! 第三道防线的一半：**硬链接别名**检测。
//!
//! 为什么单独一个文件：三个平台分支各带一段长论证（Unix 为什么 fail-closed、Windows 为什么
//! fail-open、目录为什么必须豁免），而 `agent_tools.rs` 棘轮余量为 0。搬出来是纯位置移动，
//! 判据与调用点都没变（唯一调用点仍是 `resolve_readable`）。
//!
//! 🔴 **别把这里的策略与 [`crate::retrieval::is_sensitive_path`] 合并**：那个按**名字**判，
//! 这个按**文件身份**判 —— `ln .env notes.md` 之后名字与 canonicalize 结果都是 `notes.md`，
//! 前者一定放行。两道判的是不同维度，缺一道那条绕过就成立。

use super::is_sensitive_path;
use std::path::Path;
/// 若 `real` 存在任一**敏感命名**的硬链接别名，返回那个敏感名字；否则 `None`。
///
/// 硬链接绕过的本质：`.env` 与 `notes.md` 指向同一 inode 时，读 `notes.md` 等于读 `.env`，
/// 而路径字符串与 canonicalize 结果都是 `notes.md` —— 名字/落点两道判定全部放行。
/// 唯一可靠判据是比对**文件身份**：这里直接枚举同卷上指向同一文件记录的所有路径名
/// （Win32 `FindFirstFileNameW`/`FindNextFileNameW`），逐个过 [`is_sensitive_path`]。
///
/// **保守 fail-open**：枚举 API 本身失败（非 NTFS 卷、权限不足、路径过长）时返回 `None`。
/// 理由：这是名字/落点两道判定**之后**的第三道加固，前两道仍在；且枚举失败通常意味着底层
/// 文件系统根本不支持硬链接、本无此攻击面。宁可放行也不把正常读路径在边缘环境上全拦死。
///
/// 实现策略：优先 `FindFirstFileNameW` 精确枚举所有别名（只拒有敏感名的）；该 API 不可用时
/// （某些环境返回 `ERROR_NOT_SUPPORTED`）退化为查硬链接数 `nNumberOfLinks`，多链接一律
/// fail-closed（与 Unix 策略对齐）。
#[cfg(windows)]
pub(super) fn sensitive_hardlink_alias(real: &Path) -> Option<String> {
    // 先尝试精确枚举（最优）
    if let Some(alias) = try_enumerate_hardlinks(real) {
        return Some(alias);
    }

    // 枚举不可用时退化为链接数检测（fail-closed）
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = File::open(real).ok()?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandle(
            HANDLE(file.as_raw_handle() as *mut _),
            &mut info,
        )
        .is_ok()
    };

    if ok && info.nNumberOfLinks > 1 {
        Some(format!(
            "nNumberOfLinks={} (无法枚举别名，为安全起见拒绝所有多链接文件)",
            info.nNumberOfLinks
        ))
    } else {
        None
    }
}

/// 尝试用 `FindFirstFileNameW` 精确枚举硬链接别名。成功时返回第一个敏感名，失败返回 `None`。
#[cfg(windows)]
fn try_enumerate_hardlinks(real: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        GetLastError, ERROR_HANDLE_EOF, ERROR_MORE_DATA, HANDLE, MAX_PATH,
    };
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileNameW, FindNextFileNameW,
    };

    // FindXxxFileNameW 返回的是「卷内相对路径」（如 \path\to\.env），不含盘符。
    // 敏感判定只看**叶子文件名**，卷内相对路径足够取到叶子名，无需拼回盘符。

    // FindFirstFileNameW 不支持 \\?\ 前缀（canonicalize 返回的扩展路径）。
    // 需要 strip 掉该前缀后再传给 API。
    let path_str = real.to_string_lossy();
    let normalized = path_str.strip_prefix(r"\\?\").unwrap_or(&path_str);

    let wide: Vec<u16> = normalized.encode_utf16().chain(std::iter::once(0)).collect();

    // 缓冲区长度（字符数）。先给 MAX_PATH，不够时按 ERROR_MORE_DATA 提示的长度重来。
    let mut len: u32 = MAX_PATH;
    let mut buf: Vec<u16> = vec![0u16; len as usize];

    // SAFETY: wide 以 NUL 结尾；buf 容量 = len；handle 成功后必定 FindClose。
    let handle: HANDLE = loop {
        let mut try_len = len;
        let r = unsafe {
            FindFirstFileNameW(PCWSTR(wide.as_ptr()), 0, &mut try_len, PWSTR(buf.as_mut_ptr()))
        };
        match r {
            Ok(h) => {
                break h;
            }
            Err(_) => {
                // 缓冲不足：try_len 被写成所需长度，扩容重试一次。其它错误一律放行。
                let err = unsafe { GetLastError() };
                if err == ERROR_MORE_DATA && try_len > len {
                    len = try_len;
                    buf = vec![0u16; len as usize];
                    continue;
                }
                // API 不可用（ERROR_NOT_SUPPORTED 等），返回 None 让调用方退化到链接数检测
                return None;
            }
        }
    };

    let mut found: Option<String> = None;
    loop {
        // 当前 buf 里是一个卷内相对路径（NUL 结尾）。取叶子名判敏感。
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let rel_path = std::ffi::OsString::from_wide(&buf[..end]);
        if is_sensitive_path(Path::new(&rel_path)) {
            let leaf = Path::new(&rel_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_path.to_string_lossy().into_owned());
            found = Some(leaf);
            break;
        }
        // 下一条别名。
        let mut next_len = len;
        let r = unsafe {
            FindNextFileNameW(handle, &mut next_len, PWSTR(buf.as_mut_ptr()))
        };
        if r.is_err() {
            let e = unsafe { GetLastError() };
            if e == ERROR_MORE_DATA && next_len > len {
                len = next_len;
                buf = vec![0u16; len as usize];
                // 重试当前这一条（FindNextFileNameW 缓冲不足时不推进游标）。
                let mut retry_len = len;
                if unsafe {
                    FindNextFileNameW(handle, &mut retry_len, PWSTR(buf.as_mut_ptr()))
                }
                .is_err()
                {
                    break;
                }
                continue;
            }
            // ERROR_HANDLE_EOF = 枚举结束（正常）；其它错误保守停止。
            let _ = ERROR_HANDLE_EOF;
            break;
        }
    }

    // SAFETY: handle 来自成功的 FindFirstFileNameW。
    unsafe {
        let _ = FindClose(handle);
    }
    found
}

/// macOS/Unix：标准库能可靠拿到 inode 的链接数，但不能从 inode 反查同文件系统上的
/// 全部路径名。对**常规文件** `nlink > 1` 时无法证明其它名字里没有 `.env`/密钥文件，故 fail-closed。
///
/// **目录必须豁免**：Unix 目录的 nlink 天然大于 1（= 子目录数 + 2），macOS 上
/// `.`/`src`/`sub` 这类普通目录实测 nlink=2~6。若对目录也判 nlink>1，只读工具连
/// 普通目录都拒 —— 这正是本次 mac CI 抓到的 3 条回归（list_dir / grep / read_file
/// 全因「路径 `.` 被拒 nlink=6」失败）。
///
/// 这不是理论攻击面：`ln .env notes.md` 无需任何特权，canonicalize(notes.md) 仍是
/// notes.md，按「输入名 + 真实落点」两次敏感判定都会放行。复制文件会得到独立 inode，
/// 用户确有读取需求时可复制一份；安全工具不该拿无法验证的别名碰运气。
#[cfg(unix)]
pub(super) fn sensitive_hardlink_alias(real: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::metadata(real).ok()?;
    // 只防「常规文件被硬链接别名」。目录、符号链接（前面 canonicalize 已处理）、
    // 特殊文件（socket/FIFO/设备）不在凭据别名攻击面内。
    if !md.is_file() {
        return None;
    }
    let nlink = md.nlink();
    (nlink > 1).then(|| format!("nlink={nlink}"))
}

/// 其它非 Windows/非 Unix 平台（当前 Tauri 桌面目标不会走到）：保守拒绝无法获取元数据的情况
/// 会误伤所有文件，故维持 no-op；新增平台时必须显式实现并补测试。
#[cfg(not(any(windows, unix)))]
pub(super) fn sensitive_hardlink_alias(_real: &Path) -> Option<String> {
    None
}
