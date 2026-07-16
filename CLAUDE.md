# SynaRoute 项目须知

Tauri 2 桌面应用（Rust 后端 `src-tauri/` + React/TS 前端）。代理路由 AI API 请求，含故障转移、模型映射、协议转换、加密密钥存储、健康检查。

## ⚠️ 构建/部署硬规则（踩过坑，务必遵守）

**生产 exe 必须用 `tauri build`，禁止裸 `cargo build --release`。**

裸 `cargo build` 不会嵌入前端资源，产出的 exe 运行时去连 `localhost:1420`（devUrl），生产环境无 dev server → `ERR_CONNECTION_REFUSED`，界面打不开。

```bash
npm run tauri build              # 出 NSIS 安装包（交付）
npm run tauri build -- --no-bundle   # 只出 exe（快速验证）
```

**部署前必须用可证伪证据验证前端已嵌入**：`dist/assets/` 的 chunk 名要能在产物 exe 里 `grep -c` 到（> 0）。裸 cargo build 产物该值为 0。

完整流程、验证判据、部署步骤见 [docs/04-构建部署指南.md](docs/04-构建部署指南.md)。

## 其他硬规则

- 改配置/二进制前必先备份（带日期后缀，可回滚）。
- 禁止把本机路径硬编码进代码；路径一律动态解析（面向通用用户）。
- 运行数据：`%APPDATA%\SynaRoute\{config.json, secrets.enc}`。

## 文档

- [docs/01-需求规格说明书.md](docs/01-需求规格说明书.md)
- [docs/02-技术架构设计文档.md](docs/02-技术架构设计文档.md)
- [docs/03-UIUX设计文档.md](docs/03-UIUX设计文档.md)
- [docs/04-构建部署指南.md](docs/04-构建部署指南.md) ← 构建部署坑点与流程
