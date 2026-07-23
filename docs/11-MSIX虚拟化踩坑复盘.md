# 11 - MSIX AppData 虚拟化踩坑复盘（平行宇宙惨案）

> 2026-07-23 定案。此文是「用户 UI 显示 4 个 Key、Claude 反复验证 6 个 Key」多日排查的完整复盘。
> 结论先行：**不是应用 bug，是开发环境（Claude 桌面 MSIX 包）的文件系统虚拟化陷阱。**

## 一、现象

- 磁盘 `%APPDATA%\SynaRoute\config.json`（Claude 视角）始终 6 条 Key；用户双击 exe 打开 UI 始终 4 条（cunai、luckyg 稳定消失）。
- Claude 做的六层探针（磁盘→后端内存→IPC→前端 bridge→zustand→React 渲染）**每层都是 6 条**，用户仍看到 4 条。
- 同一个 `F:\SynaRoute\synaroute.exe`：Claude 脚本启动 = 6 条；用户资源管理器双击 = 4 条。
- 「启动自检」日志对比（同路径、同用户名、同 exe）：Claude 启动 `keys=6`，用户启动 `keys=4`。
- 决定性物证：`C:\Users\<user>\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\SynaRoute\config.json` 存在——Claude 一直读写的是**这个包内私有副本**。

## 二、根因

Claude 桌面应用是 **MSIX 打包**（包家族名 `Claude_pzs8sxrjxfjjc`）。Windows 对包内进程实施 **AppData 虚拟化（写时复制 COW）**：

- **Claude Code 及它派生的所有子进程**（Bash、powershell、node、由它启动的 SynaRoute 实例）带包身份运行，读写 `%APPDATA%\SynaRoute\*` 被系统**透明重定向**到包容器私有副本 `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\SynaRoute\*`。
- **用户在资源管理器双击 exe** 无包身份，读写真实的 `%APPDATA%\SynaRoute\*`。
- 首次包内写入时 COW 复制当时的真实文件到私有副本，此后**两份文件各自演化、互不可见**——两个平行宇宙。

本案时间线：两代 Key id 都创建于 7/19（相差约 1 小时）——用户曾在两个宇宙里各建过一次 Key；真实宇宙后来剩 4 条，虚拟宇宙保有 6 条（含 cunai/luckyg）。

## 三、为什么排查绕了远路（错误归因清单）

| 错误归因 | 为何看似成立 | 为何是错的 |
|---|---|---|
| 旧版本进程残留（陈旧内存） | 清进程+重装后一度「变 6」 | 那次「6」是 Claude 启动的实例（虚拟宇宙）；用户再开（真实宇宙）又 4 |
| WebView2 HTTP 缓存旧前端 | 清缓存后首开 6、重开 4，像缓存命中 | 首开是 Claude 启动（虚拟=6），重开是用户启动（真实=4），与缓存无关 |
| 安装包不同步 / 裸 cargo build | 历史上真踩过该坑 | chunk 可证伪核对全过，包没问题 |

**方法论教训**：所有「验证」都在 Claude 自己的文件系统视角里完成，验证得越充分越自欺。当出现「我这里全对、用户那里就是不对」时，第一嫌疑是**两边看到的不是同一份数据**（虚拟化/重定向/权限视角），而不是数据流丢失。

## 四、逃逸与修复手段（可复用）

### 4.1 逃逸包身份读写真实文件：计划任务

计划任务服务启动的进程**无包身份**，看真实文件系统：

```bash
# Git Bash 下需禁路径转换（注意：会同时破坏 taskkill //F 的斜杠写法，用完 unset）
export MSYS_NO_PATHCONV=1
schtasks /Create /TN SynaTask /TR "F:\SynaRoute\job.bat" /SC ONCE /ST 23:59 /F
schtasks /Run /TN SynaTask
schtasks /Delete /TN SynaTask /F
```

- bat 文件与输入输出一律放**非 AppData 盘**（F:\、E:\ 等不被虚拟化，Claude 直接读写即真实字节）。
- 读真实文件 = bat 里 `copy` 到 F:\ 再由 Claude 读；写真实文件 = Claude 写到 F:\ 再由 bat `copy` 回 `%APPDATA%`。

### 4.2 交付验证铁律

1. **必须让用户亲自双击启动**验收；Claude 自己启动的实例活在虚拟宇宙，其表现不代表用户。
2. 用共享的 **exe 同级 `logs\` 目录**（非 AppData，不被虚拟化，两宇宙实例写同一份）核对「启动自检」行：

   ```
   启动自检 · 配置=<路径> · keys=<N> · 用户=<user> · exe=<路径>
   ```

   该日志在 `lib.rs` Store::init 后落盘（append_event），**为此而设，勿删**。

### 4.3 本次数据合并（已执行）

以用户真实配置（4 条）为基底，从虚拟副本移植 cunai/luckyg 两条 Key 及其 DPAPI 密文（同用户跨宇宙可解密），合并为 6 条经计划任务写回真实路径；写回前真实文件已备份 `F:\SynaRoute\backup\real-config.bak-20260723-112638`（及 secrets、premerge-snapshot）。用户启动自检 `keys=6` 验收通过。

### 4.4 禁忌

- **不要删除包容器里的虚拟副本**（`Packages\...\LocalCache\Roaming\SynaRoute`）：可能留下 tombstone 反而遮蔽真实文件。留着无害。
- 不要把「Claude 视角读到的 %APPDATA%」当作用户数据做迁移/清理决策。

## 五、复发速查清单

出现以下任一症状，直接按本文处理，不要重走数据流排查：

- [ ] 同一 exe：用户开和 Claude 开，数据不同
- [ ] Claude 端到端探针全对，用户界面仍旧
- [ ] `%APPDATA%\SynaRoute` 目录清单在 Claude 视角与计划任务视角不同（文件数/大小/mtime 对不上）
- [ ] `Packages\Claude_*\LocalCache\Roaming\SynaRoute` 下存在副本

→ 用 §4.1 逃逸读真实文件对比，用 §4.2 以用户宇宙为准验收。
