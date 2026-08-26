//! 内置厂商种子：新建 Key 时选中厂商即自动填好 base_url + 协议 + 一组预设模型。
//!
//! 从 `model.rs` 抽出来的（那边冻结在棘轮上、余量为 0），但也该独立：这是一张**数据表**，
//! 而 model.rs 是类型定义。
//!
//! # 数据是怎么来的（2026-08-26 逐家核对，不是文档推测）
//!
//! 每条 `base_url` 都做过**可证伪的探测**：拿一个 bogus key 打该路径，
//! **401/403 = 路由存在**、**404 = 不存在**。这个手法能区分「地址对但没鉴权」与
//! 「地址压根错了」，比读文档可靠 —— 好几家的文档自身就不自洽（Novita 的 SDK 示例
//! 与 curl 示例 base 不一样；Cohere 的 chat 示例用 `.ai` 而 audio 示例用 `.com`）。
//!
//! # 几条会让「一键导入」直接失败的坑（都已按实测处置）
//!
//! - **路径段顺序**：Groq 是 `/openai/v1`，DeepInfra 是 `/v1/openai` —— 恰好相反，抄错即 404。
//! - **Fireworks 的模型 id 把小数点写成字母 `p`**（GLM 5.2 → `glm-5p2`），
//!   且必须带完整 `accounts/fireworks/models/` 三段前缀。写 `glm-5.2` 必 404。
//! - **SiliconFlow 的 `Pro/` 前缀**：同一份权重有免费档与 `Pro/` 付费档两个 id，
//!   多写或少写一个 `Pro/` 就 404，而它不会告诉你原因。
//! - **Together 的模型 id 大小写敏感**，且与 Novita 同款权重的 id 大小写不同
//!   （`zai-org/GLM-5.2` vs `zai-org/glm-5.2`）—— 跨平台照抄 id 是 404 的头号来源。
//! - **Azure 的鉴权头是 `api-key:` 而不是 `Authorization: Bearer`**，且请求体的 `model`
//!   要填「你的部署名」。故它的 base_url 只能给模板（含 `{RESOURCE_NAME}` 占位），
//!   不能当成开箱可用项。
//!
//! # 拿不到权威来源的一律留空，不编
//!
//! `context_window: None` 的意思是「未取证」，不是「没有限制」。两家如此：
//! - **Mistral** 现在全站不 publish 现役模型的 context 长度；
//! - **Meta Llama API** 文档整站在登录墙后面（模型 id 也拿不到，故 `preset_models` 为空）。
//!
//! 编一个数的代价很具体：用户一键导入后每次请求都 400/404，而报错指向「你的配置不对」。

use crate::model::{PresetModel, Protocol, Vendor};

/// 造一条厂商。`models` 用 [`pm`] / [`pm_unknown`] 构造。
fn mk(
    id: &str,
    name: &str,
    url: &str,
    proto: Protocol,
    models: Vec<PresetModel>,
) -> Vendor {
    Vendor {
        id: id.into(),
        name: name.into(),
        default_base_url: url.into(),
        default_protocol: proto,
        builtin: true,
        icon: None,
        preset_models: models,
    }
}

/// 一个预设模型（带已取证的 context window）。
fn pm(real: &str, disp: &str, ctx: u32) -> PresetModel {
    PresetModel {
        real_name: real.into(),
        display_name: Some(disp.into()),
        context_window: Some(ctx),
    }
}

/// 一个预设模型，**context window 未取证**。
///
/// `None` 的语义是「不知道」，不是「无限制」。界面据此不显示窗口大小，
/// 而不是显示一个编出来的数字 —— 后者会让用户按错的上限去规划长上下文请求。
fn pm_unknown(real: &str, disp: &str) -> PresetModel {
    PresetModel {
        real_name: real.into(),
        display_name: Some(disp.into()),
        context_window: None,
    }
}

/// 内置厂商种子。
///
/// **顺序即界面顺序**：国际原厂 → 国内原厂 → 聚合/中转 → 本机推理 → 自定义。
/// `custom` 必须排最后（它是「以上都不是」的兜底项）。
pub fn builtin_seed() -> Vec<Vendor> {
    use Protocol::{Anthropic, OpenaiChat, OpenaiResponses};
    vec![
        // ==================== 国际原厂 ====================
        // 实测 POST /v1/messages 返 401 authentication_error（路由存在）。
        // 5 系三个都是 1M context；4.5/4.6 系已被官方列入 Legacy（context 只有 200k），
        // 故预设换成 5 系 —— 上一版表里的 claude-opus-4-5 / sonnet-4-5 已过时。
        mk(
            "anthropic",
            "Anthropic",
            "https://api.anthropic.com",
            Anthropic,
            vec![
                pm("claude-opus-5", "Claude Opus 5", 1_000_000),
                pm("claude-sonnet-5", "Claude Sonnet 5", 1_000_000),
                pm("claude-fable-5", "Claude Fable 5", 1_000_000),
                pm("claude-haiku-4-5", "Claude Haiku 4.5", 200_000),
            ],
        ),
        // /v1/chat/completions 与 /v1/responses 实测双双 401（两条路由都在）。
        // ⚠️ 1,050,000 是官方口径的「总上下文窗口」；同一页另列「最大输入 922,000 /
        // 最大输出 128,000」。这里按项目习惯填总窗口。
        mk(
            "openai",
            "OpenAI",
            "https://api.openai.com/v1",
            OpenaiResponses,
            vec![
                pm("gpt-5.6-sol", "GPT-5.6 Sol", 1_050_000),
                pm("gpt-5.6-terra", "GPT-5.6 Terra", 1_050_000),
                pm("gpt-5.6-luna", "GPT-5.6 Luna", 1_050_000),
            ],
        ),
        // Gemini 的 OpenAI 兼容端点。base 末段是 `openai`（不是版本段），
        // 代理会拼成 `.../v1beta/openai/v1/chat/completions` —— 比官方文档多一个 /v1，
        // **但实测这条也通**（带 bogus key 返 400「Please pass a valid API key」而非 404）。
        mk(
            "gemini",
            "Google Gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            OpenaiChat,
            vec![
                pm("gemini-3.7-flash", "Gemini 3.7 Flash", 1_048_576),
                pm("gemini-3.6-flash", "Gemini 3.6 Flash", 1_048_576),
                pm("gemini-3.5-flash", "Gemini 3.5 Flash", 1_048_576),
                pm("gemini-3.1-pro-preview", "Gemini 3.1 Pro", 1_048_576),
            ],
        ),
        // 实测返 400 invalid-argument「Incorrect API key provided」（路由存在）。
        // context 取文档模型页原文口径（500k / 1M / 256k）。
        mk(
            "xai",
            "xAI Grok",
            "https://api.x.ai/v1",
            OpenaiChat,
            vec![
                pm("grok-4.6", "Grok 4.6", 500_000),
                pm("grok-4.5", "Grok 4.5", 500_000),
                pm("grok-4.3", "Grok 4.3", 1_000_000),
            ],
        ),
        // ⚠️ context 全部**未取证**：Mistral 文档站现在不 publish 任何现役模型的
        // context 长度（models overview / lifecycle / pricing / API reference 四页都查过）。
        mk(
            "mistral",
            "Mistral",
            "https://api.mistral.ai/v1",
            OpenaiChat,
            vec![
                pm_unknown("mistral-large-latest", "Mistral Large"),
                pm_unknown("mistral-medium-latest", "Mistral Medium"),
                pm_unknown("mistral-small-latest", "Mistral Small"),
            ],
        ),
        // compatibility 端点。文档里 `.ai` 与 `.com` 两个 host 混用，实测**都返 401**
        // （即都可用）；取 `.ai` 是因为 chat 示例用的是它。
        mk(
            "cohere",
            "Cohere",
            "https://api.cohere.ai/compatibility/v1",
            OpenaiChat,
            vec![
                pm("command-a-plus-05-2026", "Command A+", 128_000),
                pm("command-a-03-2025", "Command A", 256_000),
                pm("command-a-reasoning-08-2025", "Command A Reasoning", 256_000),
            ],
        ),
        // 🔴 **不能填 https://api.perplexity.ai**：那个形态是「SDK 自己接 /chat/completions」，
        // 与本项目「base + path」的拼法结构性不兼容，在这里必然 404。
        // 用它的 router 端点（同时也能路由到别家模型）。
        mk(
            "perplexity",
            "Perplexity",
            "https://api.perplexity.ai/router/v1",
            OpenaiChat,
            vec![
                pm_unknown("perplexity/sonar", "Sonar"),
                pm_unknown("perplexity/sonar-pro", "Sonar Pro"),
            ],
        ),
        // 🔴 **preset_models 刻意为空**：Meta 的开发者文档整站在登录墙后面
        // （四个路径都返 200 但正文是登录壳），模型 id 与 context 都拿不到权威来源。
        // base_url 是实测过的（返 OpenAI 形状的 401 authentication_error）。
        mk(
            "meta-llama",
            "Meta Llama API",
            "https://api.llama.com/compat/v1",
            OpenaiChat,
            vec![],
        ),
        // ==================== 国内原厂 ====================
        mk(
            "deepseek",
            "DeepSeek 深度求索",
            "https://api.deepseek.com",
            OpenaiChat,
            vec![
                pm("deepseek-chat", "DeepSeek Chat", 128_000),
                pm("deepseek-reasoner", "DeepSeek Reasoner", 128_000),
            ],
        ),
        mk(
            "zhipu",
            "智谱 GLM",
            "https://open.bigmodel.cn/api/paas/v4",
            OpenaiChat,
            vec![
                pm("glm-4.6", "GLM-4.6", 200_000),
                pm("glm-4.5", "GLM-4.5", 128_000),
                pm("glm-4.5-air", "GLM-4.5-Air", 128_000),
            ],
        ),
        mk(
            "moonshot",
            "月之暗面 Kimi",
            "https://api.moonshot.cn/v1",
            OpenaiChat,
            vec![
                pm("kimi-k2-0905-preview", "Kimi K2", 256_000),
                pm("moonshot-v1-128k", "Moonshot v1 128k", 128_000),
            ],
        ),
        // 阿里通义千问：走 DashScope 的**兼容模式**路径（`/compatible-mode/v1`），
        // 不是 DashScope 原生的 `/api/v1/services/...`（那套是自有协议，本代理不认）。
        // 实测 401 `Incorrect API key provided`（阿里云 model-studio 的错误页）。
        // 国际站是另一个域名 `dashscope-intl.aliyuncs.com`，同样实测 401 —— 海外账号要手改域名。
        mk(
            "qwen",
            "通义千问 Qwen（阿里云百炼）",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            OpenaiChat,
            vec![
                pm_unknown("qwen-max", "Qwen Max"),
                pm_unknown("qwen-plus", "Qwen Plus"),
                pm_unknown("qwen-turbo", "Qwen Turbo"),
            ],
        ),
        // 字节豆包：火山方舟 Ark v3。实测 401 `AuthenticationError`（带 Request id）。
        // 🔴 **最大的坑不在地址而在 `model` 字段**：Ark 上很多账号需要填「推理接入点 ID」
        // （形如 `ep-20260101120000-xxxxx`）而不是模型名，两者都可能被接受、取决于账号开通方式。
        // 故这里的预设只给模型名形态，用户若报 `model not found` 要去 Ark 控制台取接入点 ID
        // 填进「真实模型名」。context 未取证 → 一律 None，不编。
        mk(
            "doubao",
            "字节豆包（火山方舟）",
            "https://ark.cn-beijing.volces.com/api/v3",
            OpenaiChat,
            vec![
                pm_unknown("doubao-seed-1-6", "Doubao Seed 1.6"),
                pm_unknown("doubao-1-5-pro-32k", "Doubao 1.5 Pro 32k"),
            ],
        ),
        // MiniMax：三个域名实测**都返 401**（`api.minimaxi.com` / `api.minimax.chat` /
        // `api.minimax.io`），错误体一字不差，说明是同一后端的三个入口。
        // 取 `minimaxi.com`（其现行文档主域）；海外账号可手改成 `.io`。
        mk(
            "minimax",
            "MiniMax 稀宇",
            "https://api.minimaxi.com/v1",
            OpenaiChat,
            vec![
                pm_unknown("MiniMax-M2", "MiniMax M2"),
                pm_unknown("MiniMax-Text-01", "MiniMax Text 01"),
            ],
        ),
        // 腾讯混元。实测 401，错误体是 OpenAI 形状（`Incorrect API key provided: sk-sy***`）。
        mk(
            "hunyuan",
            "腾讯混元",
            "https://api.hunyuan.cloud.tencent.com/v1",
            OpenaiChat,
            vec![
                pm_unknown("hunyuan-turbos-latest", "混元 TurboS"),
                pm_unknown("hunyuan-t1-latest", "混元 T1"),
            ],
        ),
        // 百度文心：千帆 **v2** 才是 OpenAI 兼容形态（v1 是百度自有协议 + access_token 参数）。
        // 实测 401 `invalid_iam_token` —— 注意它的报错说的是 IAM token，
        // 也就是说这里要填的是千帆的 **API Key（bearer）**，不是 AK/SK 对。
        mk(
            "ernie",
            "百度文心 ERNIE（千帆）",
            "https://qianfan.baidubce.com/v2",
            OpenaiChat,
            vec![
                pm_unknown("ernie-4.5-turbo-128k", "文心 4.5 Turbo 128k"),
                pm_unknown("ernie-x1-turbo-32k", "文心 X1 Turbo 32k"),
            ],
        ),
        // 阶跃星辰 StepFun。实测 401 `invalid_api_key`。
        mk(
            "stepfun",
            "阶跃星辰 StepFun",
            "https://api.stepfun.com/v1",
            OpenaiChat,
            vec![
                pm_unknown("step-2-16k", "Step-2 16k"),
                pm_unknown("step-1o-turbo-vision", "Step-1o Turbo Vision"),
            ],
        ),
        // 讯飞星火。实测 401，但报错是 `HMAC signature cannot be verified: apikey not found`
        // —— 它的「OpenAI 兼容」入口仍在校验 HMAC 形态的凭据，用户要填的是控制台的
        // **APIPassword**（不是 APIKey:APISecret 那一对）。填错时的报错不会说这件事。
        mk(
            "spark",
            "讯飞星火",
            "https://spark-api-open.xf-yun.com/v1",
            OpenaiChat,
            vec![pm_unknown("4.0Ultra", "星火 4.0 Ultra"), pm_unknown("generalv3.5", "星火 V3.5")],
        ),
        // 零一万物 Yi。实测 401 `Illegal ApiKey`。
        mk(
            "lingyiwanwu",
            "零一万物 Yi",
            "https://api.lingyiwanwu.com/v1",
            OpenaiChat,
            vec![pm_unknown("yi-lightning", "Yi Lightning"), pm_unknown("yi-large", "Yi Large")],
        ),
        // 百川。实测 401（错误体照抄 OpenAI，连 `platform.openai.com` 的指路都留着）。
        mk(
            "baichuan",
            "百川智能",
            "https://api.baichuan-ai.com/v1",
            OpenaiChat,
            vec![pm_unknown("Baichuan4-Turbo", "Baichuan4 Turbo"), pm_unknown("Baichuan4-Air", "Baichuan4 Air")],
        ),
        // 无问芯穹 Infini-AI（国内多模型托管）。实测 401 `请使用正确的api key进行请求`。
        mk(
            "infini",
            "无问芯穹 Infini-AI",
            "https://cloud.infini-ai.com/maas/v1",
            OpenaiChat,
            vec![],
        ),
        // ==================== 聚合 / 中转 / 托管 ====================
        // 取证最强的一家：模型 id 与 context 直接来自它自己的公开接口
        // https://openrouter.ai/api/v1/models（无需鉴权，实测返 417 个模型）。
        mk(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            OpenaiChat,
            vec![
                pm("anthropic/claude-opus-5", "Claude Opus 5", 1_000_000),
                pm("anthropic/claude-sonnet-5", "Claude Sonnet 5", 1_000_000),
                pm("moonshotai/kimi-k3", "Kimi K3", 1_048_576),
                pm("z-ai/glm-5.3", "GLM-5.3", 1_048_576),
                pm("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro", 1_048_576),
            ],
        ),
        // ⚠️ base 是 `/openai/v1`（openai 在前、v1 在后）—— 与 DeepInfra 恰好相反。
        // 实测**没有** Anthropic 端点（/anthropic/v1/messages 与 /openai/v1/messages 都 404），
        // 所以 Groq 只能走本代理做协议转换，不能给 Claude Code 直连。
        mk(
            "groq",
            "Groq",
            "https://api.groq.com/openai/v1",
            OpenaiChat,
            vec![
                pm("openai/gpt-oss-120b", "GPT-OSS 120B", 131_072),
                pm("openai/gpt-oss-20b", "GPT-OSS 20B", 131_072),
                pm("minimaxai/minimax-m2.7", "MiniMax M2.7", 196_608),
            ],
        ),
        // ⚠️ 模型 id **大小写敏感**，且与 Novita 同款权重的写法不同
        // （这里 `zai-org/GLM-5.2` 大写，Novita 是全小写）。跨平台照抄必 404。
        mk(
            "together",
            "Together AI",
            "https://api.together.ai/v1",
            OpenaiChat,
            vec![
                pm("moonshotai/Kimi-K3", "Kimi K3", 1_048_576),
                pm("zai-org/GLM-5.2", "GLM-5.2", 1_000_000),
                pm("deepseek-ai/DeepSeek-V4-Pro", "DeepSeek V4 Pro", 512_000),
                pm("openai/gpt-oss-120b", "GPT-OSS 120B", 128_000),
            ],
        ),
        // ⚠️ 两个必踩的坑：① 必须带完整 `accounts/fireworks/models/` 三段前缀；
        // ② slug 里的**小数点写成字母 p**（GLM 5.2 → `glm-5p2`）。写 `glm-5.2` 必 404。
        mk(
            "fireworks",
            "Fireworks AI",
            "https://api.fireworks.ai/inference/v1",
            OpenaiChat,
            vec![
                pm("accounts/fireworks/models/kimi-k3", "Kimi K3", 1_048_576),
                pm(
                    "accounts/fireworks/models/deepseek-v4-pro",
                    "DeepSeek V4 Pro",
                    1_048_576,
                ),
                pm("accounts/fireworks/models/glm-5p2", "GLM-5.2", 1_048_576),
            ],
        ),
        // ⚠️ base 是 `/v1/openai`（v1 在前、openai 在后）—— 与 Groq 恰好相反。
        // 实测**没有** Responses API（/v1/openai/responses 返 404）。
        mk(
            "deepinfra",
            "DeepInfra",
            "https://api.deepinfra.com/v1/openai",
            OpenaiChat,
            vec![
                pm("moonshotai/Kimi-K3", "Kimi K3", 1_048_576),
                pm(
                    "deepseek-ai/DeepSeek-V4-Pro-0813",
                    "DeepSeek V4 Pro",
                    1_048_576,
                ),
                pm("zai-org/GLM-5.2", "GLM-5.2", 1_048_576),
            ],
        ),
        // 它自己文档不自洽（SDK 示例不带 v1、curl 带 v1），实测三种都能路由；
        // 取文档 curl 那条最稳。
        mk(
            "novita",
            "Novita AI",
            "https://api.novita.ai/openai/v1",
            OpenaiChat,
            vec![
                pm("zai-org/glm-5.2", "GLM-5.2", 1_000_000),
                pm("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro", 1_000_000),
                pm("minimax/minimax-m3", "MiniMax M3", 1_000_000),
            ],
        ),
        // ⚠️ `Pro/` 前缀是这家独有的 404 源：同一份权重有免费档与 `Pro/` 付费档两个 id，
        // 多写或少写一个就 404，而它不会告诉你原因。
        mk(
            "siliconflow",
            "硅基流动 SiliconFlow",
            "https://api.siliconflow.cn/v1",
            OpenaiChat,
            vec![
                pm("deepseek-ai/DeepSeek-V4-Flash", "DeepSeek V4 Flash", 1_048_576),
                pm("moonshotai/Kimi-K2.7-Code", "Kimi K2.7 Code", 262_144),
                pm("Pro/zai-org/GLM-5.2", "GLM-5.2 (Pro)", 1_048_576),
            ],
        ),
        // 主机是裸 aihubmix.com（**没有** api. 前缀）。
        mk(
            "aihubmix",
            "AiHubMix",
            "https://aihubmix.com/v1",
            OpenaiChat,
            vec![
                pm("claude-sonnet-4-5", "Claude Sonnet 4.5", 200_000),
                pm("claude-opus-4-5", "Claude Opus 4.5", 200_000),
                pm("gemini-3.5-flash", "Gemini 3.5 Flash", 1_048_576),
            ],
        ),
        // 🔴 base_url 是**用户专属**的（`{RESOURCE_NAME}` 要换成自己的资源名），
        // 且两条差异会让「一键导入」直接失败，必须让用户知道：
        // ① 鉴权头是 `api-key:` 而不是 `Authorization: Bearer`；
        // ② 请求体的 `model` 要填「你的部署名」而不是模型名。
        // 故 preset_models 留空 —— 给一组模型名反而会误导。
        mk(
            "azure-openai",
            "Azure OpenAI（需填自己的资源名）",
            "https://{RESOURCE_NAME}.openai.azure.com/openai/v1",
            OpenaiChat,
            vec![],
        ),
        // ==================== 本机推理 ====================
        mk(
            "ollama",
            "Ollama（本机）",
            "http://localhost:11434/v1",
            OpenaiChat,
            vec![],
        ),
        mk(
            "lmstudio",
            "LM Studio（本机）",
            "http://localhost:1234/v1",
            OpenaiChat,
            vec![],
        ),
        // ==================== 兜底 ====================
        // 必须排最后：它是「以上都不是」。
        mk("custom", "自定义", "", Anthropic, vec![]),
    ]
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 每条内置厂商的**结构性**判据。这几条界面上都看不出来，而各自对应一个真实失效：
    /// base_url 写错 → 用户一键导入后每次请求都 404，而报错指向「你的密钥或地址不对」。
    #[test]
    fn every_builtin_vendor_is_well_formed() {
        let seed = builtin_seed();
        assert!(seed.len() > 15, "内置厂商数量看起来不对：{}", seed.len());

        let mut ids = std::collections::HashSet::new();
        for v in &seed {
            assert!(v.builtin, "{} 应标记为内置", v.id);
            assert!(ids.insert(v.id.clone()), "厂商 id 重复：{}", v.id);
            assert!(!v.name.trim().is_empty(), "{} 缺显示名", v.id);

            // `custom` 是「以上都不是」的兜底项，它的 base_url 刻意为空。
            if v.id == "custom" {
                assert!(v.default_base_url.is_empty(), "custom 的 base_url 应为空");
                continue;
            }

            let url = &v.default_base_url;
            assert!(
                url.starts_with("http://") || url.starts_with("https://"),
                "{} 的 base_url 没有协议前缀：{url}",
                v.id
            );
            // 结尾的斜杠会让代理拼出 `//chat/completions` —— 多数上游能容忍，
            // 但有几家会 404，而那时用户看到的是「地址不对」，方向完全错。
            assert!(!url.ends_with('/'), "{} 的 base_url 不应以 / 结尾：{url}", v.id);
            // 本机推理用 http，其余一律 https（明文发密钥不可接受）
            if !url.contains("localhost") && !url.contains("127.0.0.1") {
                assert!(url.starts_with("https://"), "{} 应用 https：{url}", v.id);
            }
            // 已知会被拼错的两个形态：路径里不该出现重复的版本段
            assert!(!url.contains("/v1/v1"), "{} 的 base_url 有重复版本段：{url}", v.id);
        }

        // `custom` 必须存在且**排在最后**（它是兜底项，排前面会让用户先看到它）
        assert_eq!(
            seed.last().map(|v| v.id.as_str()),
            Some("custom"),
            "custom 必须是最后一条"
        );
    }

    /// 预设模型的判据。
    ///
    /// `context_window: None` 的语义是「**未取证**」，不是「无限制」——
    /// 两家如此（Mistral 不 publish、Meta 文档在登录墙后）。这条测试允许 None，
    /// 但不允许 `Some(0)`：那是个会被界面当真的假数字。
    #[test]
    fn preset_models_have_sane_shapes() {
        for v in builtin_seed() {
            let mut names = std::collections::HashSet::new();
            for m in &v.preset_models {
                assert!(
                    !m.real_name.trim().is_empty(),
                    "{} 有空的模型 real_name",
                    v.id
                );
                assert!(
                    names.insert(m.real_name.clone()),
                    "{} 的预设模型 {} 重复",
                    v.id,
                    m.real_name
                );
                if let Some(ctx) = m.context_window {
                    assert!(
                        ctx >= 4_096,
                        "{} 的 {} context={ctx} 太小，看着像笔误",
                        v.id,
                        m.real_name
                    );
                }
                // 显示名给了就不能是空串（空串会让界面显示一个看不见的选项）
                if let Some(d) = &m.display_name {
                    assert!(!d.trim().is_empty(), "{} 的 {} 显示名是空串", v.id, m.real_name);
                }
            }
        }
    }

    /// 那几家**已知会 404** 的 id 形态必须保持原样。
    ///
    /// 这条不是洁癖：这些形态每一个都是实测踩出来的 404 源，而「顺手把它改成看起来
    /// 更整齐的样子」是极自然的举动 —— 改完之后用户一键导入的每个请求都 404，
    /// 而错误信息不会告诉他原因。
    #[test]
    fn known_404_traps_stay_encoded() {
        let seed = builtin_seed();
        let by = |id: &str| seed.iter().find(|v| v.id == id).expect(id).clone();

        // Fireworks：小数点写成字母 p，且必须带三段前缀
        let fw = by("fireworks");
        assert!(
            fw.preset_models
                .iter()
                .all(|m| m.real_name.starts_with("accounts/fireworks/models/")),
            "Fireworks 的模型 id 必须带完整 accounts/fireworks/models/ 前缀，否则 404"
        );
        assert!(
            fw.preset_models.iter().any(|m| m.real_name.ends_with("glm-5p2")),
            "Fireworks 把小数点写成字母 p（glm-5p2）；写 glm-5.2 必 404"
        );

        // Groq 与 DeepInfra 的路径段顺序恰好相反 —— 抄错即 404
        assert!(
            by("groq").default_base_url.ends_with("/openai/v1"),
            "Groq 是 /openai/v1（openai 在前）"
        );
        assert!(
            by("deepinfra").default_base_url.ends_with("/v1/openai"),
            "DeepInfra 是 /v1/openai（v1 在前）—— 与 Groq 相反"
        );

        // SiliconFlow 的 Pro/ 前缀：同一权重有免费档与付费档两个 id
        assert!(
            by("siliconflow")
                .preset_models
                .iter()
                .any(|m| m.real_name.starts_with("Pro/")),
            "SiliconFlow 的付费档必须带 Pro/ 前缀"
        );

        // Together 的 id 大小写敏感，且与 Novita 同款权重写法不同
        let tg = by("together");
        let nv = by("novita");
        assert!(
            tg.preset_models.iter().any(|m| m.real_name == "zai-org/GLM-5.2"),
            "Together 是大写 GLM-5.2"
        );
        assert!(
            nv.preset_models.iter().any(|m| m.real_name == "zai-org/glm-5.2"),
            "Novita 是小写 glm-5.2 —— 与 Together 不同，跨平台照抄必 404"
        );

        // Perplexity 不能用裸 api.perplexity.ai（与本项目的 base+path 拼法不兼容）
        assert_eq!(
            by("perplexity").default_base_url,
            "https://api.perplexity.ai/router/v1",
            "Perplexity 必须用 router 端点；裸 api.perplexity.ai 在本项目里必然 404"
        );

        // AiHubMix 的主机没有 api. 前缀
        assert!(
            by("aihubmix").default_base_url.starts_with("https://aihubmix.com/"),
            "AiHubMix 的主机是裸 aihubmix.com（没有 api. 前缀）"
        );

        // Azure 的 base_url 是模板，必须留着占位符（否则用户会以为开箱可用）
        assert!(
            by("azure-openai").default_base_url.contains("{RESOURCE_NAME}"),
            "Azure 的 base_url 是用户专属的，必须保留 {{RESOURCE_NAME}} 占位符"
        );
    }

    /// 未取证的地方必须**留空**，不许被后来的人「顺手填上」一个编出来的数。
    #[test]
    fn unverified_data_stays_empty() {
        let seed = builtin_seed();
        let by = |id: &str| seed.iter().find(|v| v.id == id).expect(id).clone();

        // Mistral：全站不 publish 现役模型的 context 长度
        assert!(
            by("mistral").preset_models.iter().all(|m| m.context_window.is_none()),
            "Mistral 的 context 未取证，必须留 None —— 填一个数会让用户按错的上限规划请求"
        );
        // Meta Llama：文档整站在登录墙后面，模型 id 拿不到
        assert!(
            by("meta-llama").preset_models.is_empty(),
            "Meta Llama 的模型 id 未取证，preset_models 必须为空"
        );
        // Azure：模型名要填「部署名」，给一组模型名反而误导
        assert!(
            by("azure-openai").preset_models.is_empty(),
            "Azure 的 model 字段要填部署名，不该给预设模型名"
        );
    }

    /// 每个内置厂商都应当能被前端的品牌图标匹配到（否则界面上是个首字母块，
    /// 而我们明明知道它是哪家）。
    ///
    /// 判据放在这里而不是前端：厂商表是 Rust 侧的事实来源，而 `brandIcons.ts`
    /// 的关键词是前端的。两边分叉的表现是「新加的厂商没有图标」—— 静默。
    /// 前端那侧有 `brandKeywordsDoNotCollide` 管冲突，这一侧管**覆盖**。
    ///
    /// ⚠️ 判据必须复刻 `resolveBrand` 的**真实语义**（关键词是 vendor id 的子串），
    /// 不能图省事写成「id 在文件里出现过」—— 第一版就是那么写的，于是
    /// `meta-llama`（由 `meta`/`llama` 关键词命中）与 `azure-openai`（由 `azure` 命中）
    /// 被误报成缺失，而真正缺的那一个（`aihubmix`）混在里面看不出来。
    #[test]
    fn every_vendor_id_appears_in_the_frontend_brand_keywords() {
        let brands = include_str!("../../src/components/brandIcons.ts");
        // 把所有 `keywords: [...]` 里的字符串抠出来，复刻前端的「关键词是子串」匹配。
        let mut keywords: Vec<String> = Vec::new();
        let mut rest = brands;
        while let Some(i) = rest.find("keywords: [") {
            rest = &rest[i + "keywords: [".len()..];
            let Some(end) = rest.find(']') else { break };
            for tok in rest[..end].split(',') {
                let t = tok.trim().trim_matches('"').trim();
                if !t.is_empty() {
                    keywords.push(t.to_ascii_lowercase());
                }
            }
            rest = &rest[end..];
        }
        // 反向判据：抠不出关键词说明上面的解析坏了，别让门空转
        assert!(
            keywords.len() > 40,
            "只解析出 {} 个关键词 —— 判据在空转，先修解析",
            keywords.len()
        );

        // `custom` 是兜底项、没有品牌
        const NO_BRAND: &[&str] = &["custom"];
        let mut missing = Vec::new();
        for v in builtin_seed() {
            if NO_BRAND.contains(&v.id.as_str()) {
                continue;
            }
            let id = v.id.to_ascii_lowercase();
            if !keywords.iter().any(|kw| id.contains(kw.as_str())) {
                missing.push(v.id.clone());
            }
        }
        assert!(
            missing.is_empty(),
            "这些内置厂商在前端 brandIcons.ts 的关键词里匹配不到，界面上会显示成首字母块：{missing:?}"
        );
    }
}
