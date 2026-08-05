//! **`upstream` 对外契约守卫**（P2-1 目录化的安全绳）。
//!
//! `upstream` 正在被拆成 `upstream/` 下的多个子模块。拆分承诺是
//! 「对外的 `crate::upstream::X` 路径一个都不变」—— 外部有 40 多处引用，
//! 任何一处路径变了都是破坏性改动。这里把**全部对外名字**逐个引用一遍，
//! 少了任何一条 `pub use`，编译立刻失败。
//!
//! ## 为什么这个文件在 upstream **外面**
//!
//! 最初把它写在 `upstream/mod.rs` 里，那是错的：Rust 的私有项对**定义模块及其后代**可见，
//! 所以放在 upstream 内部的守卫连 `apply_auth` 这种私有函数都能引用成功 ——
//! 它验证的是「名字存在」，而不是「对外可见」，恰恰漏掉了要守的那件事。
//!
//! 挪到 crate 根之后才是真正的外部视角：这里引用得到的，proxy.rs / aggregate.rs
//! 也一定引用得到。
//!
//! 顺带查出三个名字其实**不需要对外**：`apply_auth` / `parse_openai_tool_call` /
//! `is_retriable_upstream_error` / `SseDirection` —— 它们在 upstream 之外只出现在注释里，
//! 或者根本不需要被命名（SseDirection 只作为 sse_direction 的返回值被解构后直接传参）。
//! 这类「看着像对外 API、其实没人用」的名字只有在拆分时才会暴露出来。

/// 类型：用 `Option<T>` 引用，避免要求它们实现 Default 或可构造。
#[test]
fn public_types_stay_reachable() {
    let _: Option<crate::upstream::TokenUsage> = None;
    let _: Option<crate::upstream::SseTranslator> = None;
    let _: Option<crate::upstream::ImagePart> = None;
    let _: Option<crate::upstream::MultimodalPrompt> = None;
    let _: Option<crate::upstream::ToolDef> = None;
    let _: Option<crate::upstream::ToolInvocation> = None;
    let _: Option<crate::upstream::ToolResultMsg> = None;
    let _: Option<crate::upstream::ToolSession> = None;
    let _: Option<crate::upstream::TurnOutcome> = None;
    let _: Option<crate::upstream::TurnParams> = None;
}

/// 函数：取函数项本身，不调用（有些要网络、有些是 async）。
#[test]
fn public_functions_stay_reachable() {
    let _ = crate::upstream::fetch_models;
    let _ = crate::upstream::health_probe;
    let _ = crate::upstream::health_probe_real;
    let _ = crate::upstream::text_completion;
    let _ = crate::upstream::extract_usage;
    let _ = crate::upstream::join_endpoint;
    let _ = crate::upstream::shared_client;
    let _ = crate::upstream::sse_direction;
    let _ = crate::upstream::collect_tool_namespaces;
    let _ = crate::upstream::collect_custom_tools;
    let _ = crate::upstream::collect_search_tools;
    let _ = crate::upstream::convert_request_owned;
    let _ = crate::upstream::convert_response_ext;
    // with_usage 是泛型 async：给足类型参数太啰嗦，用一个具体调用点证明它可达即可。
    let _fut = crate::upstream::with_usage(async {});
}
