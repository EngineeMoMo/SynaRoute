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
