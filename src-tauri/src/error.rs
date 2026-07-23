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
        // 展开错误源链：reqwest 顶层信息常是笼统的「error sending request for url (…)」，
        // 真因（operation timed out / connection refused / dns error / tls…）藏在 source 链里。
        // 曾因此把「非流式补全超过 30s 单请求超时」误读成上游不可达，排障半天——必须展开。
        let mut msg = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(s) = src {
            msg.push_str(&format!(" ← {s}"));
            src = s.source();
        }
        AppError::Upstream(msg)
    }
}

pub type AppResult<T> = Result<T, AppError>;
