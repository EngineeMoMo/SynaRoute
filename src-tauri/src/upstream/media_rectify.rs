//! 文本模型收到图片时的**被动**降级：上游因图片报错 → 把图片块换成占位文本 → 同 Key 重发。
//!
//! # 为什么是被动（等报错），不是主动（预判能力）
//!
//! cc-switch 有两条路：主动按 provider 声明的模型能力预先剥图，被动按上游报错剥图重试。
//! 主动那条我们**做不了** —— SynaRoute 拉取的模型信息（`ModelInfo`）只有
//! `real_name/source/context_window/max_output_tokens`，**没有 vision/modality 字段**，
//! 上游 `/v1/models` 也普遍只返回模型名。硬做只能让用户手维护一张能力表，配错就静默剥图。
//! 被动这条零配置、对**任何**第三方中转都成立 —— 判据是上游自己的报错，不是我们的猜测。
//!
//! # 🔴 替换成占位文本，不是删除
//!
//! 删掉图片块后如果 `content` 空了，上游会因「content 不能为空」再报一次 400 —— 正是
//! `thinking_rectify::strip_thinking_blocks` 反复踩的「修一个 400 换来另一个 400」。故一律
//! **替换**成一个文本块（模型至少知道「这里本来有张图」），消息结构不塌。
//!
//! # 图片块形态**由上游协议决定**
//!
//! 剥图作用于**转换后的 payload**（已是 `key.protocol` 的形态），故按上游协议认三种：
//! - Anthropic：`{"type":"image","source":{…}}`
//! - OpenAI Chat：`{"type":"image_url","image_url":{…}}`
//! - OpenAI Responses：`{"type":"input_image","image_url":…}`
//!
//! # 落点：与预算整流同一处（转发函数内、非 2xx、同 Key 重发一次）
//!
//! 见 `proxy.rs` 两个转发函数里 `rectify_thinking_budget(...) || rectify_on_image_rejection(...)`
//! 的守卫。`||` 短路保证每趟最多一个整流命中、总共只重发一次；两者判据互斥
//! （预算查 `budget_tokens`，图片查 image/vision + unsupported），顺序不影响。
//! **不挂候选循环前段**：那对单候选无效，而用户可能只配一条文本模型 Key
//! （同 2026-09-21 预算整流从前段搬进转发函数的理由）。

use crate::model::{CategoryType, ProviderKey};
use crate::store::Store;
use serde_json::{json, Value};

/// 占位文本：图片被剥掉后模型看到的东西。点明「上游不支持」，让模型知道不是用户没发。
const IMAGE_PLACEHOLDER: &str = "[图片未发送：当前上游模型不支持图片输入]";

/// 这条上游错误是否属于「模型不接受图片」。
///
/// 判据抄 cc-switch `proxy/media_sanitizer.rs::is_unsupported_image_error`（实测清单）：
/// 状态码由调用方过（400/415/422/501 —— 这里只看文本，状态码在 proxy 侧已是失败分支），
/// 文本分两类命中：
/// 1. **自证短语**（不要求提到 image）：`only support text` / `only supports text`
///    —— 火山方舟报 `Model only support text input`，全程不含 image（cc-switch issue #5025）。
/// 2. **提到媒体字样** + **「不支持」短语**：前者 image/vision/multimodal/modality/media/
///    attachment 之一，后者 unsupported/not supported/text only/invalid content type 等。
///
/// 大小写无关的子串匹配（文案由各家上游自己拼，没有稳定机器码）。失效方向是**退回现状**
/// （不剥、照旧报错），不会误伤正常请求。
pub(crate) fn is_unsupported_image_error(upstream_err: &str) -> bool {
    let e = upstream_err.to_ascii_lowercase();
    // 场景 1：自证短语。
    if e.contains("only support text") || e.contains("only supports text") {
        return true;
    }
    // 场景 2：必须先提到媒体字样。
    let mentions_media = ["image", "vision", "multimodal", "multi-modal", "modality", "modalities", "media", "attachment"]
        .iter()
        .any(|k| e.contains(k));
    if !mentions_media {
        return false;
    }
    // 再命中「不支持」短语。
    [
        "unsupported", "not supported", "does not support", "doesn't support",
        "do not support", "don't support", "text only", "text-only",
        "invalid content type", "invalid message content", "unknown variant",
        "unknown content type", "unrecognized content type", "cannot process",
        "cannot handle", "can't process", "can't handle", "unable to process",
    ]
    .iter()
    .any(|k| e.contains(k))
}

/// 这个 content 块是不是图片块（三种上游协议形态之一）。
fn is_image_block(b: &Value) -> bool {
    matches!(
        b.get("type").and_then(Value::as_str),
        Some("image") | Some("image_url") | Some("input_image")
    )
}

/// 把一个图片块替换成占位文本块，**迁移原块的 `cache_control`**。
///
/// 迁移 cache_control 的理由同 cc-switch：图片块可能正好是某个缓存断点的载体，
/// 换成文本时若丢掉它，前缀缓存断点就没了 → 断缓存命中（上游是 Anthropic 时尤其明显）。
fn placeholder_for(block: &Value) -> Value {
    let mut out = json!({ "type": "text", "text": IMAGE_PLACEHOLDER });
    if let Some(cc) = block.get("cache_control") {
        out["cache_control"] = cc.clone();
    }
    out
}

/// 结果：剥了多少个图片块。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Stripped {
    pub images: usize,
}

/// 遍历 `messages[].content[]`（数组型 content）把图片块替换成占位文本。
///
/// **幂等**：没有图片块时返回 0 且不改 payload —— 调用方不需要「是否已剥」的标志，重发后
/// 再遇同错也无块可剥、返回 false，天然终止。
fn replace_content(content: &mut Value, text_type: &str) -> usize {
    let Some(blocks) = content.as_array_mut() else { return 0 };
    let mut count = 0;
    for block in blocks {
        if is_image_block(block) {
            *block = placeholder_for(block);
            block["type"] = json!(text_type);
            count += 1;
        } else if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            if let Some(nested) = block.get_mut("content") {
                count += replace_content(nested, "text");
            }
        }
    }
    count
}

pub(crate) fn strip_image_blocks(payload: &mut Value) -> Stripped {
    let mut out = Stripped::default();
    for (field, text_type) in [("messages", "text"), ("input", "input_text")] {
        if let Some(messages) = payload.get_mut(field).and_then(Value::as_array_mut) {
            for msg in messages {
                if let Some(content) = msg.get_mut("content") {
                    out.images += replace_content(content, text_type);
                }
                if msg.get("type").and_then(Value::as_str) == Some("function_call_output") {
                    if let Some(output) = msg.get_mut("output") {
                        out.images += replace_content(output, "input_text");
                    }
                }
            }
        }
    }
    out
}

/// 失败分支上的整流：命中「模型不支持图片」就剥图，并落一条可见事件。返回是否真的改了 payload。
///
/// **不落事件就等于没做** —— 用户看到「先 400、重试却好了但回答没提到我的图」，
/// 若日志不说为什么，那是又一个静默行为。
pub(crate) fn rectify_on_image_rejection(
    status: u16,
    upstream_err: &str,
    payload: &mut Value,
    store: &Store,
    category: CategoryType,
    key: &ProviderKey,
) -> bool {
    if !matches!(status, 400 | 415 | 422 | 501) || !is_unsupported_image_error(upstream_err) {
        return false;
    }
    let stripped = strip_image_blocks(payload);
    if stripped.images == 0 {
        return false;
    }
    store.append_event(
        category,
        // `failover` 而非 `config`：这是「为什么第一次 400、重试却好了」的唯一解释，
        // 排障的人在「故障转移」分组里找它。同签名/预算整流。
        "failover",
        Some(&key.id),
        &format!(
            "已剥除图片后重试 · {} · 移除图片块 {} 个（上游模型不支持图片输入，\
             已替换为占位文本；这一轮的图片内容未发送）",
            key.name, stripped.images,
        ),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_and_nested_tool_images_use_protocol_correct_placeholders() {
        let mut p = json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}]});
        assert_eq!(strip_image_blocks(&mut p).images, 1);
        assert_eq!(p["input"][0]["content"][0]["type"], "input_text");
        let mut p = json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"image","source":{"type":"url","url":"https://image.test/x"}}]}, {"type":"tool_use","input":{"type":"image","source":"must stay"}}]}]});
        assert_eq!(strip_image_blocks(&mut p).images, 1);
        assert_eq!(p["messages"][0]["content"][0]["content"][0]["type"], "text");
        assert_eq!(p["messages"][0]["content"][1]["input"]["type"], "image");
    }

    #[test]
    fn auth_rate_limit_and_server_errors_never_strip_images() {
        let (store, dir) = crate::service::tests::temp_store("media_status_gate");
        let key = ProviderKey::default();
        let original = json!({"messages":[{"content":[{"type":"image","source":{}}]}]});
        for status in [401,403,429,500,502,503] {
            let mut p = original.clone();
            assert!(!rectify_on_image_rejection(status,"image unsupported",&mut p,&store,CategoryType::Codex,&key));
            assert_eq!(p, original);
        }
        assert!(store.list_all_events().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_self_evident_text_only_phrase_is_recognized_without_the_word_image() {
        // 火山方舟形态：全程不含 image。
        assert!(is_unsupported_image_error("Model only support text input"));
        assert!(is_unsupported_image_error("this model only supports text"));
    }

    #[test]
    fn media_word_plus_unsupported_phrase_is_recognized() {
        assert!(is_unsupported_image_error(
            "messages.0.content.1: image is not supported by this model"
        ));
        assert!(is_unsupported_image_error(
            "invalid content type for vision: text-only model"
        ));
        assert!(is_unsupported_image_error("multimodal input unsupported"));
    }

    #[test]
    fn unrelated_errors_are_never_recognized() {
        // 只提 image 不提「不支持」→ 不命中（可能是别的关于图片的错误，剥了也没用）。
        assert!(!is_unsupported_image_error("image too large, max 5MB"));
        // 完全无关。
        assert!(!is_unsupported_image_error("rate limit exceeded"));
        assert!(!is_unsupported_image_error("invalid api key"));
        assert!(!is_unsupported_image_error("thinking.budget_tokens must be less than max_tokens"));
    }

    #[test]
    fn all_three_upstream_image_shapes_are_stripped_to_placeholder() {
        // Anthropic / Chat / Responses 三种形态混在不同消息里，都要剥。
        let mut p = json!({"messages":[
            {"role":"user","content":[
                {"type":"text","text":"看这张图"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}
            ]},
            {"role":"user","content":[
                {"type":"image_url","image_url":{"url":"https://x/y.png"}}
            ]},
            {"role":"user","content":[
                {"type":"input_image","image_url":"data:image/png;base64,BBBB"}
            ]},
        ]});
        let r = strip_image_blocks(&mut p);
        assert_eq!(r.images, 3, "三种形态都要剥");
        // 文本块保留、图片块变占位。
        assert_eq!(p["messages"][0]["content"][0]["type"], "text");
        assert_eq!(p["messages"][0]["content"][1]["type"], "text");
        assert_eq!(p["messages"][0]["content"][1]["text"], IMAGE_PLACEHOLDER);
        assert_eq!(p["messages"][1]["content"][0]["type"], "text");
        assert_eq!(p["messages"][2]["content"][0]["type"], "text");
    }

    /// 🔴 剥图**替换而非删除**：纯图消息剥完 content 不能空（否则换来 content-empty 400）。
    #[test]
    fn a_message_of_only_images_does_not_become_empty() {
        let mut p = json!({"messages":[
            {"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}
            ]}
        ]});
        strip_image_blocks(&mut p);
        let content = p["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "块数不变（替换不是删除）");
        assert_eq!(content[0]["type"], "text", "空不了");
    }

    /// 🔴 迁移 cache_control：图片块若带缓存断点，剥后占位块要继承它（否则断缓存）。
    #[test]
    fn cache_control_is_migrated_to_the_placeholder() {
        let mut p = json!({"messages":[
            {"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"},
                 "cache_control":{"type":"ephemeral"}}
            ]}
        ]});
        strip_image_blocks(&mut p);
        let block = &p["messages"][0]["content"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["cache_control"]["type"], "ephemeral", "断点必须迁移到占位块");
    }

    /// 幂等：没图片块 → 0、不改；剥完再来一遍 → 仍 0（终止性）。
    #[test]
    fn stripping_is_idempotent() {
        let clean = json!({"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]});
        let mut p = clean.clone();
        assert_eq!(strip_image_blocks(&mut p).images, 0);
        assert_eq!(p, clean, "无图片时一个字节都不改");

        let mut img = json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"hi"},
            {"type":"image","source":{"type":"url","url":"https://x"}}
        ]}]});
        assert_eq!(strip_image_blocks(&mut img).images, 1);
        assert_eq!(strip_image_blocks(&mut img).images, 0, "剥完再剥无块可剥");
    }
}
