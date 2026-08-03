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

    /// 上游请求错误。`status` = 上游 HTTP 状态码，`None` 表示**连接层失败**（没拿到响应）。
    ///
    /// 为什么要带结构化状态码：重试判定曾经靠对本消息做子串匹配
    /// （`msg.contains("HTTP 502") || … || msg.contains("连接")`），而消息里**拼进了上游响应体
    /// 前 400 字符**。于是「401 + 响应体含『请检查网络连接后重试』」这类中转商常见文案会被
    /// 判成可重试 → 白跑 3 次退避；反向「`connection reset`（不含中文『连接』）」本该重试
    /// 却被判死。状态码必须是独立字段，不能从人类可读文本里反推。
    ///
    /// `Display` 输出**刻意保持**原格式（`上游请求错误: {msg}`）——前端文案与
    /// docs 里的实测字符串都按这个格式对过，改了会连带破坏那些判据。
    #[error("上游请求错误: {msg}")]
    Upstream { status: Option<u16>, msg: String },

    #[error("代理错误: {0}")]
    Proxy(String),

    #[error("目标工具接入错误: {0}")]
    ToolConfig(String),

    #[error("无效参数: {0}")]
    Invalid(String),

    #[error("{0}")]
    Other(String),
}

impl AppError {
    /// 构造「无状态码」的上游错误（连接层失败、解析失败、密钥缺失等**没有** HTTP 响应的场景）。
    ///
    /// 提供这个构造器而非让各处写 `Upstream { status: None, msg: … }`：既让 21 个既有调用点
    /// 改动最小，也让「有状态码」变成必须显式写出来的事——避免有人图省事一律传 `None`，
    /// 那会把重试判定退回到「连接层失败一律重试」的粗粒度。
    pub fn upstream_msg(msg: impl Into<String>) -> Self {
        AppError::Upstream { status: None, msg: msg.into() }
    }

    /// 构造带 HTTP 状态码的上游错误。`status` 用于 [`crate::upstream::is_retriable_upstream_error`]
    /// 判定是否值得重试，**不要**再从 `msg` 里反推。
    pub fn upstream_http(status: u16, msg: impl Into<String>) -> Self {
        AppError::Upstream { status: Some(status), msg: msg.into() }
    }

    /// 上游 HTTP 状态码；非 `Upstream` 变体或连接层失败均返回 `None`。
    pub fn upstream_status(&self) -> Option<u16> {
        match self {
            AppError::Upstream { status, .. } => *status,
            _ => None,
        }
    }

    /// 是否为上游错误（不论有无状态码）。
    pub fn is_upstream(&self) -> bool {
        matches!(self, AppError::Upstream { .. })
    }
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
        // reqwest 错误一律是**连接层/传输层**失败（没拿到完整 HTTP 响应），故 status = None。
        // 这类失败按「临时性、值得重试」对待——与旧实现按 "error sending request" /
        // "error decoding response body" 子串匹配得出的结论一致，但不再依赖文本。
        AppError::upstream_msg(msg)
    }
}

pub type AppResult<T> = Result<T, AppError>;
