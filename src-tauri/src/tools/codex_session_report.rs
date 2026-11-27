use super::{files, SyncReport};

pub(super) fn describe(r: &SyncReport) -> Option<String> {
    let mut parts = Vec::new();
    if r.changed > 0 {
        let target = if r.target.is_empty() { "目标 provider" } else { &r.target };
        parts.push(format!(
            "已把 {} 个历史对话的全部 provider 记录指向 {target}（重启 Codex 后生效；同步不更换密钥，也不重新加密历史推理内容）",
            r.changed
        ));
    }
    if r.encrypted > 0 {
        // 只说「可能」：我们能确定的只有「这些文件里有 encrypted_content」，续聊到底会不会
        // 失败取决于上游账号，而那是我们看不到的。给出条数是为了让用户能对上号 ——
        // 上面那句条件句在没有数字时无从核实，他只会反复怀疑代理。
        parts.push(format!(
            "其中 {} 条带有原账号加密的推理内容，续聊或压缩时可能报解密失败（新建对话不受影响）",
            r.encrypted
        ));
    }
    if r.skipped > 0 {
        // 归类得出来就给精确指路，认不出才退回条件句 —— Windows 把两种成因都映射成
        // `PermissionDenied`，只有 raw OS error 32/33 能分开（见 `files::blame_from_text`）。
        let why = match r.blame {
            Some(files::IoBlame::Changed) => "（同步期间内容发生变化，已保留新内容）—— 请退出 Codex 后重新同步",
            Some(files::IoBlame::Locked) => {
                "（文件正被 Codex 占用）—— 完全退出 Codex 后再点一次接入即可"
            }
            Some(files::IoBlame::Unwritable) => {
                "（其所在目录或文件不可写）—— 退出 Codex 没用，请检查 CODEX_HOME 的权限与是否只读卷"
            }
            _ => "（文件被占用，或其所在目录不可写）—— 若 Codex 正在运行，请完全退出后再点一次接入",
        };
        parts.push(format!("{} 个对话未能同步{why}", r.skipped));
    }
    if r.unreadable > 0 {
        parts.push(format!("{} 个会话文件的首行无法解析，已跳过", r.unreadable));
    }
    if r.path_rejected > 0 {
        // 与上一条分开措辞：这里是路径本身出了问题，不是 rollout 的格式变了。
        parts.push(format!(
            "{} 个会话因路径无法安全定位而跳过（检查 CODEX_HOME 下是否有符号链接）",
            r.path_rejected
        ));
    }
    if let Some(db) = &r.sqlite {
        if db.updated > 0 || db.catalog.provider_updated > 0 {
            parts.push(format!("已校准会话库 {} 条、Desktop 本地索引 {} 条 provider 记录", db.updated, db.catalog.provider_updated));
        }
        if db.user_event_updated > 0 || db.cwd_updated > 0 {
            parts.push(format!(
                "已补齐会话索引 {} 条用户消息标记、{} 条工作目录",
                db.user_event_updated, db.cwd_updated
            ));
        }
        if let Some(e) = &db.error {
            parts.push(format!("会话库未完全同步，继续旧对话时仍可能使用旧 provider：{e}"));
        }
        // Desktop 列表索引那一份的账。清理只删索引行，不影响路由或会话数据。
        if db.catalog.inserted > 0 || db.catalog.unmarked_missing > 0 {
            parts.push(format!(
                "另修正了 Desktop 会话列表索引（补 {} 条记录、清 {} 个失效标记）",
                db.catalog.inserted, db.catalog.unmarked_missing
            ));
        }
        if db.catalog.removed > 0 {
            parts.push(format!(
                "从 Codex Desktop 本地会话索引移除 {} 条内部或侧边聊天记录（不影响路由，不删除会话、rollout 或正文）",
                db.catalog.removed
            ));
        }
        if !db.catalog.skipped.is_empty() {
            parts.push(format!("Desktop 本地会话索引清理未完全完成：{}", db.catalog.skipped.join("；")));
        }
    }
    (!parts.is_empty()).then(|| parts.join("；"))
}
