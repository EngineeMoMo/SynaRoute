//! 代理**自己应答**的非转发端点：模型发现（`/v1/models`、`/v1/models/{id}`）与
//! 官方 gateway 协议里那几个 side endpoint。
//!
//! 从 `proxy.rs` 抽出来的直接原因是那个文件棘轮余量为 0；但抽出来之后也确实更清楚 ——
//! 这三个函数与转发/故障转移没有任何交集，它们是「本地知识的只读投影」。
//!
//! # 显示名这一维（本轮新增）
//!
//! 两端都有官方的显示名通道，且都**不影响实际发送的模型名**：
//!
//! | 端 | 字段 | 实测判据 |
//! |---|---|---|
//! | Claude Code CLI | `/v1/models` 的 `display_name` | `claude.exe` 的 `TSv()` @294227923：`label: o.display_name ?? o.id` |
//! | 同上 | `/v1/models` 的 `description` | 同一处：`description: o.description ?? ""` |
//! | Claude 桌面端 | `inferenceModels[].labelOverride` | 见 `tools::desktop_profile` |
//!
//! 🔴 **`description` 给的是「对外名」而不是一句解释文案**：菜单上 `display_name` 已经把
//! 真实模型名显示出来了，此时用户唯一缺的信息是「我选中它之后，客户端发回来的是哪个 id」——
//! 那个 id 才是路由日志与 `X-SynaRoute-Decision` 里出现的名字。报障时两边能对上，
//! 靠的就是这一行。写成中文解释句反而在英文 CLI 界面里格格不入。
//!
//! 桌面端的 discovery 路径（`app.asar` @6323109）只读
//! `id`/`display_name`/`anthropic_family_tier`/`is_family_default`/`supports_1m`/`max_input_tokens`，
//! **不读 `description`** —— 故加这个字段对桌面端零影响。

use super::model_pool::advertised_pool;
use super::{error_resp, json_resp, ResBody};
use crate::model::CategoryType;
use crate::store::Store;
use bytes::Bytes;
use hyper::{Response, StatusCode};
use serde_json::Value;
use std::sync::Arc;

/// 单模型检索 `GET /v1/models/{id}`：返回**单个**模型对象（Anthropic SDK 的 models.retrieve
/// 期望的形状），查不到则按官方规范返回 404 + 标准错误信封。
///
/// 🔴 **与 [`handle_list_models`] 必须同口径**（本仓「修了 A→B，同一个坑几乎必然也在 B→A」）：
/// 显示名/description 的构造收在 [`anthropic_model_json`] 一处，两条路径都调它。
pub(super) fn handle_retrieve_model(
    store: &Arc<Store>,
    category: CategoryType,
    raw_id: &str,
) -> Response<ResBody> {
    // 去掉可能的 query（`?beta=true`）与尾斜杠；SDK 会做 URL 编码，这里解一层百分号。
    let id = raw_id.split('?').next().unwrap_or("").trim_end_matches('/');
    if id.is_empty() {
        return handle_list_models(store, category);
    }
    let models = advertised_pool(&store.enabled_keys_sorted(category));
    // 客户端可能用对外名，也可能用我们暴露的 gateway 别名（claude-synaroute-*），两者都接。
    let hit = models.iter().find(|a| {
        a.outward == id || crate::model::to_gateway_model_id(&a.outward) == id
    });
    match hit {
        Some(a) => {
            let body = if matches!(category, CategoryType::Codex) {
                serde_json::json!({ "id": a.outward, "object": "model", "owned_by": "synaroute" })
            } else {
                anthropic_model_json(a)
            };
            json_resp(StatusCode::OK, Bytes::from(serde_json::to_vec(&body).unwrap_or_default()))
        }
        None => error_resp(StatusCode::NOT_FOUND, &format!("未知模型: {id}")),
    }
}

/// 一条 Anthropic 形态的模型对象。**列表与单模型检索共用**，见 [`handle_retrieve_model`] 的 🔴。
fn anthropic_model_json(adv: &crate::model::advertise::AdvertisedModel) -> Value {
    let id = crate::model::to_gateway_model_id(&adv.outward);
    match &adv.label {
        // 有显示名：菜单显示真实模型名（或用户写的备注名），description 交出对外名。
        Some(label) => serde_json::json!({
            "type": "model",
            "id": id,
            "display_name": label,
            "description": adv.outward,
        }),
        // 无显示名 = 对外名本身就是真名（直连或恒等映射）。此时 description 是纯噪音，
        // 而 display_name 保持旧行为（对外名）—— 客户端会自己派生友好名。
        None => serde_json::json!({
            "type": "model",
            "id": id,
            "display_name": adv.outward,
        }),
    }
}

/// 官方 gateway 协议里的非推理端点，由代理本地应答。
///
/// 判据全部来自 claude.exe v2.1.219 内嵌的 llm-gateway-protocol 规范原文：
/// - `GET /managed/settings`（可选）：「Return `404` for "no managed policy"；
///   `200 {}` means "this user has an empty policy" — they're not the same」。
///   SynaRoute 不做企业策略下发，故返 404（干净的「未实现」）。
/// - `POST /v1/{metrics,logs,traces}`（可选，OTLP）：「Return `200` whether you forward or
///   discard — `404` makes the client's exporter log an error on every flush」。
///   故一律 200 丢弃，避免客户端每次 flush 刷错误日志。
///
/// 返回 `None` 表示「不是这些端点」，交由原有故障转移逻辑继续处理。
pub(super) fn handle_gateway_side_endpoints(
    method: &hyper::Method,
    path_only: &str,
) -> Option<Response<ResBody>> {
    if method == hyper::Method::GET && path_only == "/managed/settings" {
        return Some(error_resp(
            StatusCode::NOT_FOUND,
            "SynaRoute 不下发企业管控策略（managed settings 未实现）",
        ));
    }
    if method == hyper::Method::POST
        && matches!(path_only, "/v1/metrics" | "/v1/logs" | "/v1/traces")
    {
        // 明确丢弃但回 200：规范要求如此，否则客户端 OTLP exporter 每次 flush 记一条错误。
        return Some(json_resp(StatusCode::OK, Bytes::from_static(b"{}")));
    }
    None
}

/// 返回模型发现结果（GET /v1/models）。按分类协议输出对应形态：
/// - Claude CLI / 桌面端（Anthropic）：`{"data":[{"type":"model","id":..,"display_name":..}],"has_more":false}`
/// - Codex（OpenAI）：`{"object":"list","data":[{"id":..,"object":"model","owned_by":"synaroute"}]}`
pub(super) fn handle_list_models(store: &Arc<Store>, category: CategoryType) -> Response<ResBody> {
    // 模型发现不受健康态影响：只要 Key 启用，就应在 /model 选择器里列出它能服务的模型名。
    // 健康/熔断只决定「实际路由到哪个 Key」，不该决定「能选哪些模型」——否则单 Key 被真实探测
    // 判 Down 后 /model 会空掉，用户连模型都选不了（此前用 select_candidates 过滤导致的 bug）。
    let models = advertised_pool(&store.enabled_keys_sorted(category));

    // 分类固定了下游协议形态：Codex 用 OpenAI，Claude CLI/桌面端用 Anthropic。
    // Claude Code 丢弃 id 里不含 claude/anthropic 的条目 → 非合规名包成
    // `claude-synaroute-<real>`，而 display_name 走真实模型名，resolve 时剥前缀。
    let body = if matches!(category, CategoryType::Codex) {
        let data: Vec<Value> = models
            .iter()
            .map(|a| serde_json::json!({"id": a.outward, "object": "model", "owned_by": "synaroute"}))
            .collect();
        serde_json::json!({"object": "list", "data": data})
    } else {
        let data: Vec<Value> = models.iter().map(anthropic_model_json).collect();
        let first = models.first().map(|a| crate::model::to_gateway_model_id(&a.outward));
        let last = models.last().map(|a| crate::model::to_gateway_model_id(&a.outward));
        serde_json::json!({
            "data": data,
            "has_more": false,
            "first_id": first,
            "last_id": last,
        })
    };
    let bytes = Bytes::from(serde_json::to_vec(&body).unwrap_or_default());
    json_resp(StatusCode::OK, bytes)
}
