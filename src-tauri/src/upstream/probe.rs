//! 健康探测。
//!
//! 判据是**可达性**而非「/v1/models 返回 2xx」：只要拿到 HTTP 响应就说明 endpoint 活着，
//! 4xx/5xx 多是路径不支持或临时故障，真正的可用性由请求时的故障转移兜底。
//! 唯一例外是 401/403（密钥无效，留在候选池只会每次白跑一轮再转移）。

use crate::model::{ProviderKey};

use super::client::{apply_models_auth, build_client, fast_timeout};
use super::completion::text_completion;
use super::endpoint::model_endpoints;

/// 某次探测拿到的 HTTP 状态码是否代表「该 Key 可作路由候选」。
///
/// 借鉴 cc-switch 的健康判定：**可达性 ≠ 特定 API 路径返回 2xx**。只要拿到 HTTP 响应，
/// 就说明 endpoint 活着；4xx/5xx 多是路径不支持（如上游不暴露 /models 返 404/405）、
/// 限流（429）或临时故障（5xx），这些都不代表该 Key 的 chat 端点不可用——真正的可用性
/// 由请求时的故障转移兜底。唯一例外是鉴权失败（401/403）：密钥本身无效，留在候选池只会
/// 每次请求白跑一轮再转移，故直接判为不可用。
fn status_is_healthy(status: u16) -> bool {
    !matches!(status, 401 | 403)
}

/// 轻量健康探测（判「可达性」，而非旧版的「/v1/models 返回 2xx」）。
///
/// 旧实现用 `fetch_models` 判活：上游若不暴露 /models（404/405）会被误判为不可用、被路由
/// 排除，即便其 chat 端点完全正常（DeepSeek 等第三方即命中此坑）。现改为：拿到任意 HTTP
/// 响应即视为可达（鉴权失败 401/403 除外），仅连接层失败（DNS/连接/超时）判为不可达。
/// 返回 (是否健康, 延迟毫秒, 失败原因)。失败原因带出具体状态码或连接错误详情，供落日志排查——
/// 旧实现只返回 bool，日志只能打一句笼统的「连接层错误或 401/403」，无从定位。
pub async fn health_probe(key: &ProviderKey, secret: &str) -> (bool, u64, Option<String>) {
    let client = match build_client(key) {
        Ok(c) => c,
        Err(e) => return (false, 0, Some(format!("构建 HTTP 客户端失败：{e}"))),
    };
    // 用最便宜的 models 候选端点探测；只关心「有没有回应 + 状态码」，不解析 body。
    let url = model_endpoints(&key.base_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| key.base_url.trim_end_matches('/').to_string());
    let mut req = apply_models_auth(client.get(&url).timeout(fast_timeout(key)), secret);
    // Anthropic 真实 API 的 GET /v1/models 需带版本头，否则 400（不影响健康判定，但让
    // 有效 Key 能拿到真实 200 与准确延迟）。
    //
    // 这里不能用 `apply_auth`（它会按协议只设一种鉴权头），因为 `apply_models_auth` 刻意
    // **两种鉴权头都带**——兼容把 Anthropic 协议挂在子路径、而模型列表是 OpenAI 风格的厂商。
    // 但版本头仍走 Protocol 的穷举能力方法，与其余四处同源。
    if let Some((h, v)) = key.protocol.version_header() {
        req = req.header(h, v);
    }

    let start = std::time::Instant::now();
    let (healthy, reason) = match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            if status_is_healthy(code) {
                (true, None)
            } else {
                // 拿到响应但鉴权失败（401/403）：密钥无效。带出确切状态码与端点。
                (false, Some(format!("连通探测鉴权失败 HTTP {code}（GET {url}）")))
            }
        }
        // 连接层失败（超时/连不上/DNS）：带出 reqwest 错误详情。
        Err(e) => (false, Some(format!("连接层失败（GET {url}）：{e}"))),
    };
    let latency = start.elapsed().as_millis() as u64;
    (healthy, latency, reason)
}

/// 真实补全健康探测（用户开启后使用）：发一个极小的真实 completion 请求，判「业务是否真能出结果」。
///
/// 与轻量探测的区别：轻量只看端点连通性（能 ping 通就算 up），真实探测发一次最小 completion
/// （max_tokens=1、prompt 一个字），能拿到成功响应才算 up。这样「可用/熔断」与真实业务一致，
/// 消除「连通正常却熔断」的割裂——代价是消耗极少量额度（1 token 输出）。
///
/// 探测模型用 `key.probe_model()`：优先取「映射 real_name / default_model / 模型列表 / 档位」里
/// **保证被上游接受的真实模型名**——这正是真实请求经映射改写后发出去的名字，使探测与业务同路。
/// 修复了旧实现「只看 default_model+models、Key 仅配自由映射时 models 为空 → 退回轻量 /models 探测
/// → 被 401/403 误杀熔断」的 bug。都没有可探测模型时才退回轻量探测。
///
/// 返回 (是否成功, 延迟毫秒, 失败原因)。失败原因供调用方落日志——旧实现丢弃了它，导致探测
/// 失败静默、无从排查。
pub async fn health_probe_real(
    key: &ProviderKey,
    secret: &str,
    message: &str,
) -> (bool, u64, Option<String>) {
    let Some(model) = key.probe_model() else {
        // 该 Key 没有任何可探测的真实模型名 → 无法发补全，退回轻量连通探测。
        let (ok, latency, reason) = health_probe(key, secret).await;
        return (
            ok,
            latency,
            reason.map(|r| format!("无可探测模型，退回轻量连通探测：{r}")),
        );
    };
    let start = std::time::Instant::now();
    // 极小请求：一个字 prompt、max_tokens=1。不重试（探测要快、如实反映当下）。
    // 探测超时封顶 8s（fast_timeout）：1 token 秒回，不跟随用户为慢厂商设的长超时，
    // 否则一个挂掉的慢 Key 会把它所在的那条探测并发槽占满（见 health::sweep_all_enabled，
    // PROBE_CONCURRENCY = 4），拖慢整轮扫描。
    // `Some(1)`：探测**刻意**要这个上限——只验证「打得通」，不要真让上游生成内容。
    // 与大脑聚合的「不设上限」不矛盾：那边是要完整答案，这边是要秒回。
    let result =
        text_completion(key, secret, &model, message, Some(1), false, fast_timeout(key)).await;
    let latency = start.elapsed().as_millis() as u64;
    match result {
        Ok(_) => (true, latency, None),
        Err(e) => (false, latency, Some(format!("模型 {model}：{e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn health_status_treats_reachable_as_up() {
        // 拿到响应即「可达」：2xx 正常；404/405 多为不暴露该路径；429 限流；5xx 临时故障。
        // 这些都由请求时故障转移兜底，不应把 Key 踢出路由。
        for s in [200u16, 400, 404, 405, 429, 500, 502, 503] {
            assert!(status_is_healthy(s), "{s} 应判为可达");
        }
    }

    #[test]
    fn health_status_treats_auth_failure_as_down() {
        // 鉴权失败：密钥本身无效，留在候选池只会每次白跑一轮，直接判不可用。
        assert!(!status_is_healthy(401));
        assert!(!status_is_healthy(403));
    }

}
