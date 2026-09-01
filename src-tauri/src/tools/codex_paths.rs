//! Codex 根目录解析：所有 config/auth/catalog/MCP 路径的单一事实来源。
//!
//! Codex 读取路径由 `CODEX_HOME` 改变，而不是由 SynaRoute 的当前工作目录改变。之前
//! `codex.rs` 固定拼 `dirs::home_dir()/.codex`，造成「SynaRoute 写 A、Codex 读 B」的
//! 平行配置：实际 config 没有 `model_provider` 时回落 `api.openai.com`，并把 auth.json
//! 中的 `SEE-SYNA...` 占位符发给官方，正是跨机器 401 的成因。
//!
//! 解析规则刻意是 fail closed：`CODEX_HOME` 只接受非空绝对路径；相对或空值不退回默认目录。
//! 退回默认会让调用方以为接入成功，实际却仍然写错地方，比直接报错更危险。

use crate::error::{AppError, AppResult};
use std::ffi::OsString;
use std::path::PathBuf;

/// 解析 `CODEX_HOME`：空/相对值必须 fail closed，不退回默认目录。
/// 纯函数参数避免测试修改进程级环境变量，防止并发用例互相污染；`None` 表示未设置，
/// `Some(空串)` 表示设置但非法，两者不能混淆。
pub(super) fn from_env(raw: Option<OsString>) -> AppResult<PathBuf> {
    match raw {
        Some(raw) => {
            let value = raw.to_string_lossy().into_owned();
            if value.trim().is_empty() {
                return Err(AppError::ToolConfig(
                    "CODEX_HOME 已设置但为空：无法确定 Codex 实际配置目录，请修正环境变量后重试".into(),
                ));
            }
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(AppError::ToolConfig(format!(
                    "CODEX_HOME 必须是绝对路径，当前为相对路径 `{value}`；为避免写错 Codex 配置，已拒绝接入"
                )));
            }
            Ok(path)
        }
        None => {
            let home = dirs::home_dir()
                .ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
            Ok(home.join(".codex"))
        }
    }
}

/// 读取进程环境中的实际 Codex 根目录。
pub(super) fn codex_home() -> AppResult<PathBuf> {
    from_env(std::env::var_os("CODEX_HOME"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_codex_home_is_used_verbatim() {
        let got = from_env(Some(OsString::from("C:\\isolated\\codex"))).unwrap();
        assert_eq!(got, PathBuf::from("C:\\isolated\\codex"));
    }

    /// 🔴 接线判据：`codex.rs` 的路径入口必须经 [`codex_home`]，不许自己拼 `.codex`。
    ///
    /// 上面那几条只测解析函数 —— **注入实测**：把 `config_path()` 改回
    /// `dirs::home_dir().join(".codex")`，它们全部照样绿，而那就是这个缺陷本体
    /// （SynaRoute 写默认目录、Codex 从 `$CODEX_HOME` 读另一份 → 回落官方 → 占位符外发 → 401）。
    /// 在默认目录的开发机上，任何行为测试也都是绿的，这正是它潜伏这么久的原因。
    #[test]
    fn the_codex_path_entrypoints_must_go_through_codex_home() {
        let src = crate::proxy::custom_headers::production_code_only(include_str!("codex.rs"));
        let at = src.find("pub(super) fn config_path").expect("函数改名了，请同步本判据");
        let end = src[at..].find("\n}").map(|i| at + i).unwrap_or(src.len());
        let body = &src[at..end];
        assert!(
            body.contains("codex_paths::codex_home()"),
            "config_path 必须经 codex_paths::codex_home()，否则设了 CODEX_HOME 的机器上会写错目录"
        );
        assert!(
            !body.contains("home_dir()"),
            "不许在 config_path 里直接拼 dirs::home_dir() —— 那是跨机器 401 的成因"
        );
        // auth.json 必须与 config.toml 同根（它们要么一起对、要么一起错，分开推导必然漂移）。
        let at = src.find("pub(super) fn auth_path").expect("auth_path 改名了，请同步本判据");
        let end = src[at..].find("\n}").map(|i| at + i).unwrap_or(src.len());
        assert!(
            src[at..end].contains("config_path()"),
            "auth_path 必须由 config_path 派生，不许独立拼路径"
        );
    }

    #[test]
    fn an_unset_codex_home_falls_back_to_the_user_default() {
        let got = from_env(None).unwrap();
        assert!(got.ends_with(".codex"));
        assert!(got.is_absolute());
    }

    #[test]
    fn an_empty_codex_home_fails_closed_instead_of_using_the_default() {
        let err = from_env(Some(OsString::from("   "))).unwrap_err();
        assert!(err.to_string().contains("CODEX_HOME"));
        assert!(err.to_string().contains("为空"));
    }

    #[test]
    fn a_relative_codex_home_fails_closed_instead_of_resolving_against_cwd() {
        let err = from_env(Some(OsString::from(".codex-test"))).unwrap_err();
        assert!(err.to_string().contains("绝对路径"));
        assert!(err.to_string().contains("拒绝接入"));
    }
}
