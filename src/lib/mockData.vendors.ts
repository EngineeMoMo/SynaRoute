import type { Vendor } from "@/types";

/**
 * 浏览器演示模式的内置厂商清单。
 *
 * # 为什么单独一个文件、而且必须与 Rust 那份**同步**
 *
 * 真正的事实来源是 `src-tauri/src/vendors.rs::builtin_seed()`。这里这份只在
 * **没有 Tauri 后端时**（`npm run dev` 直接开浏览器、官网截图、演示站）被用到。
 *
 * 两份分叉过一次，代价不是「开发时看到的少几条」这么轻：官网 `site/public/screenshots/
 * vendors-*.png` 就是从这个页面截的，于是官网上展示的「内置厂商」是 6 条，
 * 而实际产品里是 33 条 —— **对外宣称与产品不一致**，而且没有任何东西会报错。
 *
 * 故加了一条跨语言判据 `tests/vendorSeedParity.test.ts`：id 集合与 base_url
 * 与 Rust 那份分叉就变红（编译器管不到这条缝，而分叉是静默的）。
 *
 * ⚠️ **预设模型不在判据范围内**（只比 id 与 base_url）。理由：模型清单变动频繁，
 * 把它也纳入会让「Rust 侧更新一个模型名」连带要改这里，而这份是演示数据、
 * 精度要求不同。一个**部分**为真的判据必须把边界写清楚，否则下一个人会以为全同步。
 *
 * 生成方式（不要手抄）：`node .claude/tmp-gen-mock-vendors.mjs`（一次性脚本，
 * 从 vendors.rs 的 `mk(...)` 调用里抠出来）。
 */
export const MOCK_VENDORS: Vendor[] = [
  { id: "anthropic", name: "Anthropic", defaultBaseUrl: "https://api.anthropic.com", defaultProtocol: "anthropic", builtin: true, presetModels: [
    { realName: "claude-opus-5", displayName: "Claude Opus 5", contextWindow: 1000000 },
    { realName: "claude-sonnet-5", displayName: "Claude Sonnet 5", contextWindow: 1000000 },
    { realName: "claude-fable-5", displayName: "Claude Fable 5", contextWindow: 1000000 },
    { realName: "claude-haiku-4-5", displayName: "Claude Haiku 4.5", contextWindow: 200000 },
  ] },
  { id: "openai", name: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", defaultProtocol: "openai_responses", builtin: true, presetModels: [
    { realName: "gpt-5.6-sol", displayName: "GPT-5.6 Sol", contextWindow: 1050000 },
    { realName: "gpt-5.6-terra", displayName: "GPT-5.6 Terra", contextWindow: 1050000 },
    { realName: "gpt-5.6-luna", displayName: "GPT-5.6 Luna", contextWindow: 1050000 },
  ] },
  { id: "gemini", name: "Google Gemini", defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta/openai", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "gemini-3.7-flash", displayName: "Gemini 3.7 Flash", contextWindow: 1048576 },
    { realName: "gemini-3.6-flash", displayName: "Gemini 3.6 Flash", contextWindow: 1048576 },
    { realName: "gemini-3.5-flash", displayName: "Gemini 3.5 Flash", contextWindow: 1048576 },
    { realName: "gemini-3.1-pro-preview", displayName: "Gemini 3.1 Pro", contextWindow: 1048576 },
  ] },
  { id: "xai", name: "xAI Grok", defaultBaseUrl: "https://api.x.ai/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "grok-4.6", displayName: "Grok 4.6", contextWindow: 500000 },
    { realName: "grok-4.5", displayName: "Grok 4.5", contextWindow: 500000 },
    { realName: "grok-4.3", displayName: "Grok 4.3", contextWindow: 1000000 },
  ] },
  { id: "mistral", name: "Mistral", defaultBaseUrl: "https://api.mistral.ai/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "mistral-large-latest", displayName: "Mistral Large" },
    { realName: "mistral-medium-latest", displayName: "Mistral Medium" },
    { realName: "mistral-small-latest", displayName: "Mistral Small" },
  ] },
  { id: "cohere", name: "Cohere", defaultBaseUrl: "https://api.cohere.ai/compatibility/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "command-a-plus-05-2026", displayName: "Command A+", contextWindow: 128000 },
    { realName: "command-a-03-2025", displayName: "Command A", contextWindow: 256000 },
    { realName: "command-a-reasoning-08-2025", displayName: "Command A Reasoning", contextWindow: 256000 },
  ] },
  { id: "perplexity", name: "Perplexity", defaultBaseUrl: "https://api.perplexity.ai/router/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "perplexity/sonar", displayName: "Sonar" },
    { realName: "perplexity/sonar-pro", displayName: "Sonar Pro" },
  ] },
  { id: "meta-llama", name: "Meta Llama API", defaultBaseUrl: "https://api.llama.com/compat/v1", defaultProtocol: "openai_chat", builtin: true },
  { id: "deepseek", name: "DeepSeek 深度求索", defaultBaseUrl: "https://api.deepseek.com", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "deepseek-chat", displayName: "DeepSeek Chat", contextWindow: 128000 },
    { realName: "deepseek-reasoner", displayName: "DeepSeek Reasoner", contextWindow: 128000 },
  ] },
  { id: "zhipu", name: "智谱 GLM", defaultBaseUrl: "https://open.bigmodel.cn/api/paas/v4", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "glm-4.6", displayName: "GLM-4.6", contextWindow: 200000 },
    { realName: "glm-4.5", displayName: "GLM-4.5", contextWindow: 128000 },
    { realName: "glm-4.5-air", displayName: "GLM-4.5-Air", contextWindow: 128000 },
  ] },
  { id: "moonshot", name: "月之暗面 Kimi", defaultBaseUrl: "https://api.moonshot.cn/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "kimi-k2-0905-preview", displayName: "Kimi K2", contextWindow: 256000 },
    { realName: "moonshot-v1-128k", displayName: "Moonshot v1 128k", contextWindow: 128000 },
  ] },
  { id: "qwen", name: "通义千问 Qwen（阿里云百炼）", defaultBaseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "qwen-max", displayName: "Qwen Max" },
    { realName: "qwen-plus", displayName: "Qwen Plus" },
    { realName: "qwen-turbo", displayName: "Qwen Turbo" },
  ] },
  { id: "doubao", name: "字节豆包（火山方舟）", defaultBaseUrl: "https://ark.cn-beijing.volces.com/api/v3", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "doubao-seed-1-6", displayName: "Doubao Seed 1.6" },
    { realName: "doubao-1-5-pro-32k", displayName: "Doubao 1.5 Pro 32k" },
  ] },
  { id: "minimax", name: "MiniMax 稀宇", defaultBaseUrl: "https://api.minimaxi.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "MiniMax-M2", displayName: "MiniMax M2" },
    { realName: "MiniMax-Text-01", displayName: "MiniMax Text 01" },
  ] },
  { id: "hunyuan", name: "腾讯混元", defaultBaseUrl: "https://api.hunyuan.cloud.tencent.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "hunyuan-turbos-latest", displayName: "混元 TurboS" },
    { realName: "hunyuan-t1-latest", displayName: "混元 T1" },
  ] },
  { id: "ernie", name: "百度文心 ERNIE（千帆）", defaultBaseUrl: "https://qianfan.baidubce.com/v2", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "ernie-4.5-turbo-128k", displayName: "文心 4.5 Turbo 128k" },
    { realName: "ernie-x1-turbo-32k", displayName: "文心 X1 Turbo 32k" },
  ] },
  { id: "stepfun", name: "阶跃星辰 StepFun", defaultBaseUrl: "https://api.stepfun.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "step-2-16k", displayName: "Step-2 16k" },
    { realName: "step-1o-turbo-vision", displayName: "Step-1o Turbo Vision" },
  ] },
  { id: "spark", name: "讯飞星火", defaultBaseUrl: "https://spark-api-open.xf-yun.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "4.0Ultra", displayName: "星火 4.0 Ultra" },
    { realName: "generalv3.5", displayName: "星火 V3.5" },
  ] },
  { id: "lingyiwanwu", name: "零一万物 Yi", defaultBaseUrl: "https://api.lingyiwanwu.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "yi-lightning", displayName: "Yi Lightning" },
    { realName: "yi-large", displayName: "Yi Large" },
  ] },
  { id: "baichuan", name: "百川智能", defaultBaseUrl: "https://api.baichuan-ai.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "Baichuan4-Turbo", displayName: "Baichuan4 Turbo" },
    { realName: "Baichuan4-Air", displayName: "Baichuan4 Air" },
  ] },
  { id: "infini", name: "无问芯穹 Infini-AI", defaultBaseUrl: "https://cloud.infini-ai.com/maas/v1", defaultProtocol: "openai_chat", builtin: true },
  { id: "openrouter", name: "OpenRouter", defaultBaseUrl: "https://openrouter.ai/api/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "anthropic/claude-opus-5", displayName: "Claude Opus 5", contextWindow: 1000000 },
    { realName: "anthropic/claude-sonnet-5", displayName: "Claude Sonnet 5", contextWindow: 1000000 },
    { realName: "moonshotai/kimi-k3", displayName: "Kimi K3", contextWindow: 1048576 },
    { realName: "z-ai/glm-5.3", displayName: "GLM-5.3", contextWindow: 1048576 },
    { realName: "deepseek/deepseek-v4-pro", displayName: "DeepSeek V4 Pro", contextWindow: 1048576 },
  ] },
  { id: "groq", name: "Groq", defaultBaseUrl: "https://api.groq.com/openai/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "openai/gpt-oss-120b", displayName: "GPT-OSS 120B", contextWindow: 131072 },
    { realName: "openai/gpt-oss-20b", displayName: "GPT-OSS 20B", contextWindow: 131072 },
    { realName: "minimaxai/minimax-m2.7", displayName: "MiniMax M2.7", contextWindow: 196608 },
  ] },
  { id: "together", name: "Together AI", defaultBaseUrl: "https://api.together.ai/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "moonshotai/Kimi-K3", displayName: "Kimi K3", contextWindow: 1048576 },
    { realName: "zai-org/GLM-5.2", displayName: "GLM-5.2", contextWindow: 1000000 },
    { realName: "deepseek-ai/DeepSeek-V4-Pro", displayName: "DeepSeek V4 Pro", contextWindow: 512000 },
    { realName: "openai/gpt-oss-120b", displayName: "GPT-OSS 120B", contextWindow: 128000 },
  ] },
  { id: "fireworks", name: "Fireworks AI", defaultBaseUrl: "https://api.fireworks.ai/inference/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "accounts/fireworks/models/kimi-k3", displayName: "Kimi K3", contextWindow: 1048576 },
    { realName: "accounts/fireworks/models/deepseek-v4-pro", displayName: "DeepSeek V4 Pro", contextWindow: 1048576 },
    { realName: "accounts/fireworks/models/glm-5p2", displayName: "GLM-5.2", contextWindow: 1048576 },
  ] },
  { id: "deepinfra", name: "DeepInfra", defaultBaseUrl: "https://api.deepinfra.com/v1/openai", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "moonshotai/Kimi-K3", displayName: "Kimi K3", contextWindow: 1048576 },
    { realName: "deepseek-ai/DeepSeek-V4-Pro-0813", displayName: "DeepSeek V4 Pro", contextWindow: 1048576 },
    { realName: "zai-org/GLM-5.2", displayName: "GLM-5.2", contextWindow: 1048576 },
  ] },
  { id: "novita", name: "Novita AI", defaultBaseUrl: "https://api.novita.ai/openai/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "zai-org/glm-5.2", displayName: "GLM-5.2", contextWindow: 1000000 },
    { realName: "deepseek/deepseek-v4-pro", displayName: "DeepSeek V4 Pro", contextWindow: 1000000 },
    { realName: "minimax/minimax-m3", displayName: "MiniMax M3", contextWindow: 1000000 },
  ] },
  { id: "siliconflow", name: "硅基流动 SiliconFlow", defaultBaseUrl: "https://api.siliconflow.cn/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "deepseek-ai/DeepSeek-V4-Flash", displayName: "DeepSeek V4 Flash", contextWindow: 1048576 },
    { realName: "moonshotai/Kimi-K2.7-Code", displayName: "Kimi K2.7 Code", contextWindow: 262144 },
    { realName: "Pro/zai-org/GLM-5.2", displayName: "GLM-5.2 (Pro)", contextWindow: 1048576 },
  ] },
  { id: "aihubmix", name: "AiHubMix", defaultBaseUrl: "https://aihubmix.com/v1", defaultProtocol: "openai_chat", builtin: true, presetModels: [
    { realName: "claude-sonnet-4-5", displayName: "Claude Sonnet 4.5", contextWindow: 200000 },
    { realName: "claude-opus-4-5", displayName: "Claude Opus 4.5", contextWindow: 200000 },
    { realName: "gemini-3.5-flash", displayName: "Gemini 3.5 Flash", contextWindow: 1048576 },
  ] },
  { id: "azure-openai", name: "Azure OpenAI（需填自己的资源名）", defaultBaseUrl: "https://{RESOURCE_NAME}.openai.azure.com/openai/v1", defaultProtocol: "openai_chat", builtin: true },
  { id: "ollama", name: "Ollama（本机）", defaultBaseUrl: "http://localhost:11434/v1", defaultProtocol: "openai_chat", builtin: true },
  { id: "lmstudio", name: "LM Studio（本机）", defaultBaseUrl: "http://localhost:1234/v1", defaultProtocol: "openai_chat", builtin: true },
  { id: "custom", name: "自定义", defaultBaseUrl: "", defaultProtocol: "anthropic", builtin: true },
];
