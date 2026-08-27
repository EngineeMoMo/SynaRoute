// 预览模式的事件日志样本。从 mockData.ts 抽出（那边冻结在棘轮上），
// 同 mockData.usage.ts / mockData.vendors.ts 的做法。
//
// 这些样本刻意覆盖了「用户会来问」的几种形态：故障转移、熔断、短路窗口、
// 跨协议转换失败、含 trace 的可展开行 —— 官网截图也是从预览模式截的。

import type { EventLogEntry } from "@/types";

export function mockEvents(now: number): EventLogEntry[] {
  return [
    {
      id: "e1",
      ts: now - 60000,
      categoryId: "claude-cli",
      type: "failover",
      keyId: "k1",
      keyName: "厂商1（官方直连）",
      detail: "厂商1 超时（>30s），切换到厂商2",
    },
    {
      id: "e2",
      ts: now - 59000,
      categoryId: "claude-cli",
      type: "route",
      keyId: "k2",
      keyName: "厂商2（备用中转）",
      detail: "厂商2 成功返回 opus-4-7",
      // 折叠计数：验证「×N」徽标的渲染（真实后端对连续同类成功记录会这样合并）
      repeat: 7,
      // 用量在折叠条目上是**累计值**（7 次之和）。这里刻意给一个上万的输入量，
      // 用来验证 ↑/↓ 徽标的 k 缩写与 tooltip 渲染。
      usage: { input: 128400, output: 3120, cacheRead: 96000 },
    },
    {
      id: "e3",
      ts: now - 30000,
      categoryId: "claude-cli",
      type: "health",
      keyId: "k3",
      detail: "厂商3 健康检查：状态未知（尚未探测）",
    },
    {
      id: "e3b",
      ts: now - 28000,
      categoryId: "claude-cli",
      type: "config",
      detail: "已注册 MCP 到 Claude：C:\\Users\\me\\.claude.json（http://127.0.0.1:9527/mcp），重启客户端生效",
    },
    // 这两条刻意留在 mock 里：`system`（启动自检）与 `warning`（配置告警）都曾因前端
    // TYPE_META 未登记而被兜底渲染成绿色「路由」成功——全项目最重要的诊断行反而被埋没。
    // 预览里常驻这两种事件，任何人改了登记表都能一眼看出回归。
    {
      id: "e3b1",
      ts: now - 26000,
      categoryId: "claude-cli",
      type: "system",
      detail: "启动自检 · 配置=C:\\Users\\me\\AppData\\Roaming\\SynaRoute\\config.json · keys=6 · 用户=me · exe=C:\\Program Files\\SynaRoute\\SynaRoute.exe",
    },
    {
      id: "e3b2",
      ts: now - 25000,
      categoryId: "claude-cli",
      type: "warning",
      detail: "厂商2 的余额查询地址用了 {{baseUrl}}，而接口地址带路径后缀（/anthropic）——余额端点在域名根下时应改用 {{origin}}/user/balance",
    },
    {
      id: "e3c",
      ts: now - 20000,
      categoryId: "claude-cli",
      type: "mcp",
      detail: "synaroute_ai · C:\\proj\\demo · 3个参与者 · 5个文件 · 8200ms",
      trace: {
        keyName: "synaroute_ai",
        vendor: "mcp",
        protocol: "anthropic",
        url: "-",
        requestedModel: "-",
        realModel: "-",
        requestBody: "分析当前项目的登录模块有哪些安全隐患",
        responseBody: "## 🧠 SynaRoute 多模型聚合分析\n\n1. 密码明文比对……\n2. 缺少速率限制……",
        latencyMs: 8200,
        ok: true,
      },
    },
    {
      id: "e4",
      ts: now - 15000,
      categoryId: "claude-cli",
      type: "request",
      keyId: "k2",
      detail: "厂商2 · claude-opus-4 → glm-4.6 · 1240ms",
      // 小额用量：验证 ↑/↓ 徽标在不足 1 万时不走 k 缩写（直接显示原数）。
      usage: { input: 21, output: 34 },
      trace: {
        keyName: "厂商2",
        vendor: "zhipu",
        protocol: "openai_chat",
        url: "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        requestedModel: "claude-opus-4",
        realModel: "glm-4.6",
        requestBody: JSON.stringify(
          {
            model: "glm-4.6",
            messages: [
              { role: "user", content: "用一句话解释什么是代理服务器。" },
            ],
            max_tokens: 1024,
            temperature: 0.7,
          },
          null,
          2,
        ),
        responseBody: JSON.stringify(
          {
            id: "chatcmpl-mock-abc",
            model: "glm-4.6",
            choices: [
              {
                index: 0,
                message: {
                  role: "assistant",
                  content: "代理服务器是位于客户端与目标服务器之间的中转站，代客户端转发请求并返回响应。",
                },
                finish_reason: "stop",
              },
            ],
            usage: { prompt_tokens: 21, completion_tokens: 34, total_tokens: 55 },
          },
          null,
          2,
        ),
        status: 200,
        latencyMs: 1240,
        ok: true,
      },
    },
    {
      id: "e5",
      ts: now - 8000,
      categoryId: "claude-cli",
      type: "request",
      keyId: "k1",
      detail: "厂商1 · claude-opus-4 → claude-opus-4 · 320ms · 失败 HTTP 401",
      trace: {
        keyName: "厂商1",
        vendor: "anthropic",
        protocol: "anthropic",
        url: "https://api.anthropic.com/v1/messages",
        requestedModel: "claude-opus-4",
        realModel: "claude-opus-4",
        requestBody: JSON.stringify(
          {
            model: "claude-opus-4",
            messages: [{ role: "user", content: "用一句话解释什么是代理服务器。" }],
            max_tokens: 1024,
          },
          null,
          2,
        ),
        responseBody: JSON.stringify(
          {
            type: "error",
            error: { type: "authentication_error", message: "invalid x-api-key" },
          },
          null,
          2,
        ),
        status: 401,
        latencyMs: 320,
        ok: false,
      },
    },
  ];;
}
