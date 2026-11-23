# v0.1.99 发布验收（2026-11-23）

全部审查修复已提交并推送到 `master`，正式标签为 `v0.1.99`，对应提交 `3b2fc3b1acc6e0235917166cc488b6c4b3650ed3`。GitHub Release 于 2026-11-23 08:59:24 America/New_York（13:59:24 UTC）公开发布，非草稿、非预发布，并已确认是 Latest。

- 发布页：https://github.com/EngineeMoMo/SynaRoute/releases/tag/v0.1.99
- 发布工作流：https://github.com/EngineeMoMo/SynaRoute/actions/runs/45182063903
- 修复范围：[首轮修复](security-fixes-2026-11-23.md)、[后续修复](security-followup-fixes-2026-11-23.md)。

## 云端验证

| 检查 | 结果 |
| --- | --- |
| Gates、macOS check、Performance baseline、Release | 全部 success |
| Windows Rust 完整库测试，两轮 | 每轮 1738 passed、0 failed、20 个既有 ignored |
| macOS 两个打包任务的宿主库测试 | 每任务 1720 passed、0 failed、18 个既有 ignored |
| Linux Rust 完整库测试 | 1716 passed、0 failed、18 个既有 ignored |
| 原生 Windows WebView CSP | 1 passed；资源、IPC/渲染和阻止注入等七项标志均为 true |
| 四平台正式构建 | Windows x64、macOS ARM64/Intel、Linux x64 均成功 |
| Windows 产物门 | 前端嵌入、更新签名、外泄审计、安装及启动冒烟全部通过 |

macOS Intel 产物采用交叉编译，打包任务的 Rust 库测试运行在 Apple Silicon 宿主上；没有将其表述为 Intel 原生执行验收。Linux/macOS 安装器未进行交互式真机安装；Windows NSIS 已在隔离 CI 环境实际安装并启动。

## 公开产物复验

公开下载全部 17 个资产，逐一核对 API 报告的大小，计算 SHA-256，并在 API 提供 digest 时对比。七组可更新产物均以客户端内嵌公钥重新进行 Ed25519 验签，同时验证签名里的可信注释；公钥 key ID 为 `7A46ECB8087DE26F`。

`latest.json` 的 11 项平台映射完整，与七组产物签名重新汇总的结果完全相等。公开 `releases/latest/download/latest.json` 与按版本下载的清单一致，版本为 `0.1.99`，所有包地址固定到该标签。远端注释标签解引用后与上述发布提交一致。

全部下载产物、工作流日志及机器可读验收报告保存在本地 `output/release-v0.1.99/`，未纳入源码提交。汇总文件为 `verification-report.json`。

## 未公开的尝试

- `v0.1.97`：跨平台 LAN 端口夹具失败，发布流程已主动取消。
- `v0.1.98`：第二轮 Windows 测试捕获既有会话扫描夹具的时间戳偶发，质量门阻止出包。
- 两个标签保留以追溯失败，均未公开 Release。修复后使用新标签 `v0.1.99`，没有改写已推送标签。

没有修改用户当前安装实例，没有把本地审计输出、运行时配置或构建缓存提交进仓库。依赖维护公告的剩余边界仍按首轮修复记录保留，本次发布不代表所有维护公告都已消除。
