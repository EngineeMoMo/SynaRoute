# 参考项目整合与纯文本会话测试修复

核对日期：2026-11-25，America/New_York。

## 现象与证据边界

用户截图显示 `/v1/responses` 请求被以 403 拒绝，上游消息是 `Image generation is not enabled for this group`。截图没有原始请求体，不能单凭该错误证明 SynaRoute 发出了图片生成工具声明。

本地代码确认：单 Key 会话测试原先仅构造文本消息，没有主动加入图片，但它使用 Codex 客户端标识，而且没有明确禁止工具调用。参考 sub2api 存在根据 Codex 身份自动补入 hosted image_generation 工具的路径；它支持通过 Responses Lite 标记关闭该桥接。ccLoad 也识别并转递这个 HTTP 标记。

当前参考版 sub2api 的桥接还受本地分组图片权限控制，分组禁用时不会自动补入。因此不能断言截图所在服务器就是这个版本、就是同一层网关自动注入；多级中转、旧版实现及上游模型映射仍需原始请求或服务器记录来区分。

## 已整合的修复

### 单 Key 会话测试明确限定为文本

- Responses 请求显式携带 tools=[]、tool_choice=none、parallel_tool_calls=false、text.format.type=text。
- 请求保留既有认证和兼容客户端标识，另携带 X-OpenAI-Internal-Codex-Responses-Lite=true，供支持的中转关闭图片工具桥接。
- 文本测试的标记在自定义头应用后设置，并通过真实 loopback HTTP 抓取验证：即使自定义头配置为 false，最终只有一个值为 true 的标记。
- 请求仍只包含当前会话的文本历史；普通代理转发和真实客户端的图片/工具能力不受这段测试专用配置影响。
- 若上游仍返回图片权限错误，保留原始错误并提示检查上游工具注入或模型映射；不通过自动重试或换 Key 掩盖失败。
- 中英文界面明确说明测试只用于纯文本，不启用图片生成或工具调用。

### 空函数参数兼容

借鉴 CLIProxyAPI 对历史 function_call 的空参数处理：Responses 的普通函数调用参数为空串或空白串时归一为 JSON 空对象。SynaRoute 同时覆盖 Responses → Chat 的历史请求与完整非流式响应，避免上游返回空参数后在下一轮历史重放中报错。

已有的缺失/null 参数回退保持不变。非空字符串，包括不完整 JSON，都保留原样；custom_tool_call 的裸 input 不应用该规则，工具 ID、namespace 和结果关联保持不变。本次不把未结束的流式片段补成一个成功的空调用。

### 失败状态不能作为成功答案

将 object=response、status=failed 纳入共享上游错误判定。即使 HTTP 为 200、error 为 null、output 中带有部分文本，单 Key 测试也不再把它当作完整答案。普通 completed/incomplete 响应及仅包含应用自有 status 字段的数据仍按原有规则处理。

## 对照来源

参考源码仅保存在 D:/ReferenceProject 中。本轮更新远端引用，保留参考工作区。

- CLIProxyAPI：38fe929502f19f7cce03c69c19aacddf30f1ef5e；internal/translator/codex/openai/responses/codex_openai-responses_request.go 的 normalizeEmptyFunctionCallArgumentsDoc。
- ccLoad：937874038c5037f601bc7614ca0b405d9a06cbaf；internal/app/codex_wire.go 的 codexResponsesLiteRequested、prepareCodexOAuthResponsesBody。
- sub2api：0f6671457132425d1b84f3d80536b53e02942424；backend/internal/service/openai_gateway_forward.go 的 Codex 图片桥接条件，以及 openai_codex_transform.go 的图片工具注入。

这是对适用行为的实现与验证；没有整体移植这些项目，也没有升级 Laya 或更换路由引擎。

## 验证

先增加回归并在旧实现上复现三项失败：文本测试没有明确工具禁用字段、failed 响应中的部分文本被判成功、空函数字符串参数未归一。非空/自定义参数保全对照通过。

最终结果：Rust 库测试 1,758 通过，20 项既有测试忽略；单 Key 窗口前端测试 10 通过；Clippy all-targets 且 warnings denied 通过；TypeScript/Vite 生产构建和项目质量门通过。

证据日志位于 .build-tmp/reference-integration-red.log、reference-integration-rust-tests.log、reference-integration-clippy.log、reference-integration-build.log、reference-integration-gates.log 和 reference-key-chat-frontend.log。

网络行为验证使用隔离的本地 HTTP 服务，没有消耗用户真实 Key 的额度，也没有重放截图对应的线上请求。本轮修复纳入 v0.1.103；标签触发既有多平台发布流程，安装包交付以远端构建、签名及安装器冒烟结果为准。
