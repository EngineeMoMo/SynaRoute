# macOS 安装与测试指南（SynaRoute 0.1.12）

> 适用：Apple Silicon Mac（arm64）。本版本产出的 mac 包是 **CI 构建 + ad-hoc 签名**的测试包，
> **不是**已完成 Developer ID 签名与公证的正式发布版。它用于验证 SynaRoute 在 macOS 上
> 能否编译、安装、启动、转发 —— 但首次启动会撞上 Gatekeeper，需要手动放行（见第 3 节）。

**当前版本：v0.1.13**

---

## 1. 产物清单

从 GitHub Actions 下载（run `31233219672`，artifact 名 `macos-arm64-build`）：

| 文件 | 大小 | 用途 |
|---|---|---|
| `SynaRoute_0.1.13_aarch64.dmg` | 8.7 MB | 拖拽安装包（推荐） |
| `SynaRoute-macos-arm64-app.tar.gz` | 8.7 MB | 直接解压运行的 .app（免挂载） |

获取方式（在你的 Mac 上）：
```bash
# 方式 A：GitHub 网页
# Actions → macOS check → 最新成功 run → Artifacts → macos-arm64-build

# 方式 B：命令行（需已登录 gh）
gh run download 31233219672 -n macos-arm64-build
```

产物已验证（CI 六条判据）：最低系统版本 11.0、icns 图标、arm64 架构、
前端资源嵌入、dmg 与 updater 包均存在。

---

## 2. 前置条件

- **Apple Silicon Mac**（M1/M2/M3/M4）。这是 arm64 包，Intel Mac 无法运行。
- **macOS 11.0 (Big Sur) 或更高**。低于 11 系统会拒绝启动。
- 如果本机之前装过 SynaRoute（理论上还没有，因为这是首个 mac 包），先退出旧实例。

---

## 3. 安装与首次启动

### 3.1 用 dmg 安装（推荐）

1. 双击 `SynaRoute_0.1.12_aarch64.dmg`
2. 磁盘映像挂载后，把 **SynaRoute.app** 拖进 **Applications** 文件夹
3. 打开 Finder → 应用程序 → 双击 **SynaRoute**

### 3.2 用 tar 直接运行（免挂载，最快验证）

```bash
tar -xzf SynaRoute-macos-arm64-app.tar.gz
open SynaRoute.app
```

### 3.3 ⚠️ 首次启动会被 Gatekeeper 拦 —— 必须手动放行

因为没公证，macOS 会拦截并报 **「SynaRoute 已损坏，无法打开。您应该将它移到废纸篓」**
（这是误报，不是文件真损坏）。放行方式：

**macOS 15 (Sequoia) 及之后**（Control-click 打开已被移除）：
```
系统设置 → 隐私与安全性 → 滚动到底部 →
「SynaRoute 被阻止使用」→ 点「仍要打开」→ 弹窗再点「打开」
```

**macOS 14 及之前**：
```
在 Finder 中 Control-click（右键）SynaRoute.app → 选「打开」→ 点「打开」
```

> 注：每次下载的包都需要首次放行；若之后又弹，可能是 macOS 每次更新或重新下载后
> 重新打上 quarantine 标记。终极办法是用 `xattr` 移除隔离属性：

```bash
xattr -dr com.apple.quarantine /Applications/SynaRoute.app
```

### 3.4 若仍然打不开

```bash
# 1. 确认架构
file /Applications/SynaRoute.app/Contents/MacOS/SynaRoute   # 应显示 arm64
# 2. 看崩溃报告（若闪退）
ls -t ~/Library/Logs/DiagnosticReports/ | head -3
# 3. 看是否缺权限（Gatekeeper 拦了启动）
spctl -a -vv /Applications/SynaRoute.app
```

---

## 4. 首次启动后的自检清单

这些是 mac 专属代码路径，Windows 上从没跑过。**逐条核对**，有偏差即是有 bug。

### 4.1 界面能打开、不是空白
CI 只验证了前端资源嵌进二进制，没验证运行加载。如果窗口白屏，是资源嵌入/加载问题。

### 4.2 数据落在标准目录（不是 .app 内部）

```bash
# 日志目录（应有 .log 文件）
ls -la ~/Library/Logs/SynaRoute/

# 配置目录（应有 config.json 和 secrets.enc）
ls -la ~/Library/Application\ Support/SynaRoute/

# MCP 端口文件（启动代理后出现）
cat ~/Library/Application\ Support/SynaRoute/mcp-port

# 反证：.app 内部绝不应有运行数据
find /Applications/SynaRoute.app -name "*.log" 2>/dev/null   # 应无输出
find /Applications/SynaRoute.app -name "config.json" 2>/dev/null   # 应无输出
```

### 4.3 Keychain 授权（首次存密钥时）

- 首次给某个 Key 存 API Key 时，应弹出系统弹窗
  **「SynaRoute 想使用您的钥匙串」** → 点 **始终允许**。
- 若点「拒绝」：应用应报错（提示无法读取密钥），**不应**静默退到弱加密。
- 这是 ad-hoc 签名的预期行为 —— 每次应用更新后可能再弹一次。

### 4.4 菜单栏托盘图标

- 图标应是**单色**（template），随系统深浅色自动黑/白反转。
- **鼠标悬停显示 tooltip**，内容应含运行状态（如「SynaRoute · 运行中: Claude CLI」）。
- 因为图标是单色的，运行/停止状态**只能靠 tooltip 区分** —— 这是 mac 的设计，
  不是缺陷。

### 4.5 关窗与退出行为

- 点窗口右上角关闭 → 窗口隐藏、进程留在托盘（后台继续转发），**不退出**。
- 按 `Cmd+Q` → 进程退出（这是 mac 惯例，允许）。
- 托盘右键 → 「退出」→ 进程退出。

---

## 5. 功能测试（与 Windows 一致的部分）

mac 与 Windows 共用同一套 Rust 代理引擎和前端，以下能力已由 572 条测试验证，
mac 上应行为一致：

1. **配置 Key**：厂商管理 → 新增 Key → 填 base_url/协议/密钥
2. **1M 上下文**：编辑模型 → 上下文窗口填 `1` 选 `M` → 出现「1M」徽标
   → Claude Code/CLI 用该模型时自动补 `anthropic-beta: context-1m-2025-08-07` 头
3. **故障转移**：多个 Key 时，主 Key 失败自动切备用
4. **大脑聚合**：Claude Code 里通过 `synaroute_ai` 调聚合
5. **MCP**：启用后，Codex/Claude 里能调用 synaroute 的 MCP 工具
6. **用量统计**：左侧「用量统计」面板，按分类×Key 展示 token 消耗（流式/非流式均采集）
7. **健康告警**：Key 连续失败熔断时 CategoryPage 顶部显示黄色告警条；熔断/恢复时系统通知
8. **路径可视化**：运行日志折叠态显示蓝色 Key 名徽标，不展开就能看到走了哪个 Key

---

## 6. macOS 特有的接入测试

### 6.1 Claude 桌面端接入路径

macOS 上桌面端数据目录应是 `~/Library/Application Support/Claude` 和
`~/Library/Application Support/Claude-3p`。接入后检查：

```bash
ls ~/Library/Application\ Support/Claude-3p/configLibrary/
# 应出现 SynaRoute 的档：00000000-0000-4000-8000-000053796e61.json
cat ~/Library/Application\ Support/Claude-3p/configLibrary/_meta.json
# appliedId 应指向上面那个档
```

> ⚠️ **这条最需要验证**：Windows 上桌面端目录是 `%LOCALAPPDATA%\Claude-3p`，
> macOS 上的实际目录名（是否也叫 `Claude-3p`）**尚未真机取证**。如果目录不是这个名字，
> 需要报告并调整代码。

### 6.2 Codex 接入

```bash
ls ~/.codex/config.toml        # 接入后应出现 model_provider=synaroute
ls ~/.codex/auth.json          # 接入前后字节应不变（不破坏官方登录）
```

### 6.3 只读工具路径防线（mac 专属）

在 Claude Code 里让聚合工具读工作目录里的 `.env` → **必须被拒**。
macOS 上目录 nlink>1 已被豁免，但文件硬链接仍 fail-closed。

---

## 7. 卸载

```bash
# 退出应用后
rm -rf /Applications/SynaRoute.app
rm -rf ~/Library/Application\ Support/SynaRoute
rm -rf ~/Library/Logs/SynaRoute
# 可选：清掉 Keychain 里 SynaRoute 的条目（钥匙串访问 → 找到 SynaRoute → 删除）
```

---

## 8. 已知限制（测试前请知悉）

1. **未公证**：首次启动需手动放行（第 3.3 节）。这不是安装 bug。
2. **不能自动更新**：mac CI 的 `.sig` 用一次性临时私钥签，与应用内置 pubkey 不匹配。
   点「检查更新」会验签失败 —— 这是预期的，测试包不能验证更新链路。
3. **只有 arm64**：Intel Mac 无法运行。
4. **Keychain 每次更新后可能重新弹授权**：ad-hoc 签名下 cdhash 随版本变化。

---

## 9. 测试完成后的反馈清单

如果全部通过，把结果发回；如果有偏差，报告以下信息：

| 项 | 预期 | 实测 |
|---|---|---|
| 界面打开 | 有内容，非白屏 | |
| 日志目录 | `~/Library/Logs/SynaRoute/` | |
| 配置目录 | `~/Library/Application Support/SynaRoute/` | |
| .app 内无运行数据 | `find` 无输出 | |
| Keychain 弹窗 | 首次存密钥弹「始终允许」 | |
| 托盘图标 | 单色 template | |
| Cmd+Q | 退出进程 | |
| Claude-3p 目录 | 桌面端接入路径正确 | |
| Codex auth.json | 接入前后字节不变 | |
| Gatekeeper 首次拦法 | 记录「已损坏」还是「无法验证开发者」 | |

> 特别关注第 6.1 节：`Claude-3p` 目录名在 macOS 上是否成立，是唯一未经真机取证的
> 功能未知。这一步的实测结果直接决定 `tools.rs` 的桌面端路径逻辑要不要改。
