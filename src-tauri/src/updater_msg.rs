//! 把 `tauri-plugin-updater` 的原始英文错误翻成**可行动**的中文。
//!
//! 为什么单独一个文件：`service.rs` 生产段顶着棘轮基线（1454/1454，余量 0），
//! 而这段逻辑与 service 的「编排」职责本来就不同族 —— 它是纯字符串分流，无状态、无 IO。
//! 挂载方式见 `service.rs` 里那句 `#[path]`（刻意不把 service.rs 目录化：
//! 那是 docs/15 P2-7 明确「刻意未做」的大 diff 项）。
//!
//! # 🔴 分流判据取自 updater 源码，不是猜的
//!
//! 逐行读过 `tauri-plugin-updater-2.10.1/src/updater.rs:470-528` 的 endpoint 循环，
//! 三条**语义完全不同**的失败在原实现里挤成了一句话：
//!
//! | 实际情况 | updater 内部 | 抛出的错误 |
//! |---|---|---|
//! | 请求根本没发出去（超时/连不上/DNS/TLS） | `Err(err) => last_error = Some(err.into())` | `Error::Reqwest(..)` |
//! | 发出去了但 HTTP 非 2xx（404 私有仓库） | **只 log，不设 last_error** | `Error::ReleaseNotFound` |
//! | 拿到 JSON 但没有本平台条目 | —— | `Error::TargetNotFound(..)` |
//!
//! 中间那条是原实现唯一覆盖到的，而它的归因（「仓库为私有 / 资产没上传」）**是对的** ——
//! 非 2xx 才会走到那里。真正的缺口是**另外两条**：
//!
//! - **网络失败**落到兜底分支，用户看到的是 `检查更新失败: error sending request for url (...)`。
//!   reqwest 的 `Display` **不含 source 链**，所以连「是超时还是被拒」都看不出来。
//!   而这在国内是最高频的一类（GitHub 直连常年不通），却是唯一没有中文说明的一类。
//! - **`TargetNotFound`** 同样落到兜底。它的出路与上面两条完全不同（要去补
//!   `latest.json` 的平台条目），而 2026-08-17 的 v0.1.27 就真出过这个事故：
//!   11 个平台条目只剩 6 个，Windows 用户的应用内更新静默什么都不做。
//!
//! # 签名失败为什么单独给出路
//!
//! `Error::Minisign` 最常见的真因**不是**发版签错了，而是**换钥**：已发布客户端的二进制里
//! 嵌着当时的公钥，换私钥 = 所有老用户的应用内更新永久失效（见 `secrets/README.md` 台账，
//! 这个代价已经付过两次）。原文案只说「公钥与发版签名不匹配」，读者会以为是服务端的问题、
//! 反复重试；而正确的出路只有一条：手动下载重装一次。

/// 把 updater 的原始错误文案翻成可行动的中文。
///
/// 入参是 `err.to_string()`（对 `#[error(transparent)]` 的变体就是内层错误的 `Display`），
/// 故这里只能按**文案特征**分流。每条匹配串都在模块注释的表里有对应来源。
pub(crate) fn friendly_updater_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();

    // ---- 1) 网络层：请求没发出去 / 没收到响应 ----
    // 放在最前面：它是国内最高频的一类，且与下面 404 那条的出路完全不同。
    // 匹配串覆盖 reqwest 的几种 Display 形态与被包一层的 `Error::Network(String)`。
    const NET: &[&str] = &[
        "error sending request",
        "operation timed out",
        "timed out",
        "connection refused",
        "connection reset",
        "tcp connect error",
        "dns error",
        "failed to lookup address",
        "certificate",
        "handshake",
        "网络",
    ];
    if NET.iter().any(|k| lower.contains(k)) {
        return format!(
            "连不上更新服务器（github.com）。这一步只是去取更新清单，与你的 Key 和代理设置无关。\
             常见原因与对策：① 本机到 GitHub 的网络不通 —— 打开系统代理后重试\
             （SynaRoute 会自动跟随 Windows 的系统代理设置，无需在应用内配置）；\
             ② 代理只对浏览器生效 —— 确认它开的是系统代理而非仅浏览器扩展；\
             ③ 公司网络或防火墙拦截 —— 可改为手动下载安装包。原始错误: {raw}"
        );
    }

    // ---- 2) 清单里没有本平台的条目 ----
    // 🔴 必须排在下面那条「清单取不到」**之前**：`TargetNotFound` 的原文是
    // "the platform `x` was not found in the response `platforms` object" —— 它含 `not found`,
    // 会被下面那条宽泛匹配吃掉，于是「清单缺了你的平台」被报成「仓库可能是私有的」。
    // 这个顺序依赖有 `platform_missing_is_matched_before_release_not_found` 钉着。
    //
    // 与下一条的区别：清单**取到了、也解析成功了**，只是没有当前平台/架构那一项。
    // 出路完全不同 —— 不是去改仓库可见性，而是去补 latest.json 的 platforms。
    if lower.contains("was not found in the response")
        || lower.contains("were found in the response")
        || lower.contains("unsupported application architecture")
        || lower.contains("unsupported os")
    {
        return format!(
            "更新清单里没有你这个平台/架构的条目，所以应用内更新无法进行\
             （发版时若某个平台的构建任务失败，latest.json 会缺掉那一项，\
             而其余平台不受影响）。请改为手动下载对应平台的安装包。原始错误: {raw}"
        );
    }

    // ---- 3) 清单取不到：HTTP 非 2xx，或拿到的 JSON 解析不了 ----
    if lower.contains("could not fetch a valid release json")
        || lower.contains("404")
        || lower.contains("not found")
    {
        return format!(
            "取到了响应但不是有效的更新清单 latest.json（服务器返回了非 2xx，\
             常见原因：仓库为私有导致公开 URL 返回 404；或本次发布尚未上传 Release 资产、\
             latest.json 还没生成）。请确认 Release 已发布且资产可匿名访问。原始错误: {raw}"
        );
    }

    // ---- 4) 验签失败 ----
    if lower.contains("signature") || lower.contains("minisign") {
        return format!(
            "更新包签名校验失败。最常见的原因不是服务端出错，而是本应用的版本较旧：\
             更新签名密钥换过之后，旧版本内嵌的公钥无法验证新版本的签名，\
             应用内更新会永久失效（重试多少次都一样）。\
             出路只有一条：手动下载最新安装包重装一次，之后应用内更新即恢复正常。原始错误: {raw}"
        );
    }

    format!("检查更新失败: {raw}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 四类失败必须落到**四个不同**的分支。
    ///
    /// 判据用「该分支独有的关键词」而不是整句比对：整句比对会让每次润色文案都变红,
    /// 于是维护者要么改测试要么不改文案，两条路都会让判据失去意义。
    #[test]
    fn each_failure_class_gets_its_own_actionable_message() {
        // 1) 网络：reqwest 的真实 Display 形态（不含 source 链，这正是原实现的盲点）
        let net = friendly_updater_error(
            "error sending request for url (https://github.com/o/r/releases/latest/download/latest.json)",
        );
        assert!(net.contains("连不上更新服务器"), "网络类没分流: {net}");
        assert!(net.contains("系统代理"), "网络类没给出路: {net}");

        // 2) 非 2xx / 清单取不到
        let nf = friendly_updater_error("Could not fetch a valid release JSON from the remote");
        assert!(nf.contains("latest.json"), "清单类没分流: {nf}");
        assert!(!nf.contains("连不上更新服务器"), "清单类被网络分支吃掉了: {nf}");

        // 3) 平台条目缺失（v0.1.27 真出过的事故）
        let tnf = friendly_updater_error(
            "the platform `windows-x86_64` was not found in the response `platforms` object",
        );
        assert!(tnf.contains("平台/架构的条目"), "平台类没分流: {tnf}");
        assert!(tnf.contains("手动下载"), "平台类没给出路: {tnf}");

        // 4) 验签失败 —— 必须指向「手动重装」，而不是让人怀疑服务端
        let sig = friendly_updater_error("Signature verification failed: minisign");
        assert!(sig.contains("重装"), "验签类没给出路: {sig}");
        assert!(sig.contains("版本较旧"), "验签类没说真因: {sig}");
    }

    /// 原始错误必须逐字保留 —— 排障时那句英文是唯一能对上上游文档的线索。
    #[test]
    fn the_raw_error_is_always_preserved() {
        for raw in [
            "error sending request for url (https://x)",
            "Could not fetch a valid release JSON from the remote",
            "the platform `linux-x86_64` was not found in the response `platforms` object",
            "minisign: signature mismatch",
            "某种我们没预料到的错误",
        ] {
            let msg = friendly_updater_error(raw);
            assert!(msg.contains(raw), "原始错误被吞了: raw={raw} msg={msg}");
        }
    }

    /// 认不出的错误落到兜底，但**不能**假装知道原因。
    #[test]
    fn unknown_errors_fall_through_without_inventing_a_cause() {
        let msg = friendly_updater_error("some brand new failure mode");
        assert!(msg.starts_with("检查更新失败"));
        // 兜底不许携带任何一条具体归因 —— 指错方向的告警比没有告警更糟
        for wrong in ["连不上更新服务器", "latest.json", "平台/架构的条目", "重装"] {
            assert!(!msg.contains(wrong), "兜底分支瞎归因了「{wrong}」: {msg}");
        }
    }

    /// 🔴 顺序判据：平台条目缺失必须排在「清单取不到」之前。
    ///
    /// 这条不是假想 —— 第一版就是反的，本文件的另一条测试当场把它抓了出来：
    /// `TargetNotFound` 的原文含 `not found`，被 404 那条宽泛匹配吃掉，
    /// 于是「latest.json 缺了你的平台」被报成「仓库可能是私有的」。
    /// 后者会把人送去检查仓库可见性，而那一项完全是好的。
    #[test]
    fn platform_missing_is_matched_before_release_not_found() {
        let msg = friendly_updater_error(
            "the platform `windows-x86_64` was not found in the response `platforms` object",
        );
        assert!(
            msg.contains("平台/架构的条目"),
            "同时命中两类时应判为平台条目缺失，实际: {msg}"
        );
    }

    /// 🔴 顺序判据：网络分支必须排在「清单取不到」之前。
    ///
    /// 反了会静默出错 —— reqwest 的超时文案里也可能出现 `not found`
    /// （如 `failed to lookup address information: Name or service not known`），
    /// 那时它会被 404 分支吃掉，把「网络不通」报成「仓库是私有的」，
    /// 而后者会让人跑去检查仓库可见性 —— 那一项完全是好的。
    #[test]
    fn network_is_matched_before_release_not_found() {
        let msg = friendly_updater_error(
            "error sending request: failed to lookup address information: Name or service not known",
        );
        assert!(
            msg.contains("连不上更新服务器"),
            "同时命中两类时应判为网络问题，实际: {msg}"
        );
    }
}
