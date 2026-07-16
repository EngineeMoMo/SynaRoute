use serde::Serialize;

/// 统一错误类型。实现 Serialize 以便通过 Tauri IPC 返回给前端。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("加密错误: {0}")]
    Crypto(String),

    #[error("配置未找到: {0}")]
    NotFound(String),

    #[error("上游请求错误: {0}")]
    Upstream(String),

    #[error("代理错误: {0}")]
    Proxy(String),

    #[error("目标工具接入错误: {0}")]
    ToolConfig(String),

    #[error("无效参数: {0}")]
    Invalid(String),

    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
