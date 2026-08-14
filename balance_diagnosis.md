# 🔍 SynaRoute 余额查询问题诊断报告

## 问题描述

**现象**: CC-Switch 能查询余额，SynaRoute 不能

---

## 🎯 根本原因分析

### 1. 默认配置问题 ⚠️

**位置**: `src-tauri/src/model.rs:501-520`

```rust
impl Default for BalanceQuery {
    fn default() -> Self {
        Self {
            enabled: false,  // ❌ 默认关闭！
            template: "generic".into(),
            url: "{{baseUrl}}/user/balance".into(),
            method: "GET".into(),
            auth: "bearer".into(),
            // ...
        }
    }
}
```

**问题**: 
- ❌ `enabled: false` - **余额查询默认关闭**
- 用户添加 Key 后需要**手动开启**余额查询开关
- 如果用户没有打开开关，查询会直接返回 `"未启用余额查询"`

**对比 CC-Switch**:
- ✅ CC-Switch 默认启用余额查询
- ✅ 导入 Key 后立即可用

---

### 2. 实现完整性验证 ✅

我已经验证了 SynaRoute 的余额查询实现：

#### ✅ 核心功能已实现

| 功能 | 状态 | 位置 |
|------|------|------|
| 占位符展开 | ✅ 完整 | `balance.rs:185-199` |
| 字段提取（15+候选） | ✅ 完整 | `balance.rs:32-66` |
| HTTP 请求 | ✅ 完整 | `balance.rs:261-408` |
| 客户端身份头 | ✅ 已修复 | `balance.rs:326-333` |
| Tauri 命令 | ✅ 已注册 | `lib.rs:1602` |
| 缓存机制 | ✅ 完整 | `lib.rs:283-294` |
| 并发控制 | ✅ 完整 | `lib.rs:297-308` |

#### ✅ 已知问题已修复

**问题 1**: User-Agent 缺失导致 403
```rust
// balance.rs:326-333
// **这一步不能省**：本请求是应用自建的，
// 而 `shared_client()` 不设默认 UA（reqwest 默认不发），于是部分中转渠道会把它
// 判为 `detected: unknown` 直接 403（channel:client_restricted）——
// 表现为「cc-switch 能查出余额、SynaRoute 查不出」，而两边配置一模一样。
req = crate::upstream::apply_client_identity(req, key.protocol);
```
✅ **已修复** - 代码已添加 `apply_client_identity` 调用

**问题 2**: 默认 URL 错误
```rust
// model.rs:506-510
// 此前这里写的是 `/v1/usage` —— 那是我从某个站的**自定义脚本**里读到的路径，
// 误当成了通用默认值，导致新用户一开开关就 404。对齐官方模板才是正确起点。
url: "{{baseUrl}}/user/balance".into(),
```
✅ **已修复** - URL 已对齐 cc-switch 通用模板

---

## 🔍 诊断步骤

### 步骤 1: 检查余额查询是否启用

```rust
// lib.rs:266-268
if !cfg.enabled {
    return BalanceResult::failed("未启用余额查询");
}
```

**验证方法**:
1. 打开 SynaRoute
2. 进入 Key 管理
3. 编辑某个 Key
4. 查看"余额查询"选项卡
5. 确认**开关是否打开**

---

### 步骤 2: 检查配置是否正确

**必需配置**:
```json
{
  "enabled": true,           // ✅ 必须为 true
  "url": "{{baseUrl}}/user/balance",  // 默认值，大部分站点通用
  "method": "GET",           // 默认值
  "auth": "bearer"           // 默认值，部分站点需改为 "x-api-key"
}
```

**特殊场景**:
- **NewAPI 面板**: 需要 `accessToken` + `userId`
- **不同域名**: 设置 `base_url_override`
- **自定义路径**: 修改 `url` 字段

---

### 步骤 3: 查看错误日志

**日志位置**: 
- Windows: `%APPDATA%\com.synaroute.app\logs\`
- macOS: `~/Library/Logs/com.synaroute.app/`
- Linux: `~/.local/share/com.synaroute.app/logs/`

**关键错误模式**:

| 错误信息 | 原因 | 解决方案 |
|---------|------|---------|
| "未启用余额查询" | `enabled: false` | 打开余额查询开关 |
| "未配置查询地址" | `url` 为空 | 填写查询 URL |
| "上游返回 HTTP 401" | 密钥错误/过期 | 检查 API Key |
| "上游返回 HTTP 403" | 权限不足/UA问题 | 已修复（升级到最新版） |
| "上游返回 HTTP 404" | URL 路径错误 | 检查 URL 配置 |
| "查询超时" | 网络问题/超时设置过短 | 增加 `timeout_secs` |
| "请求失败: connection" | 网络连接问题 | 检查网络/代理 |

---

## 🎯 问题根源总结

### 主要原因

**SynaRoute 不能查余额的根本原因是: `enabled` 默认为 `false`**

这是一个**设计选择差异**，而非实现缺陷：

| 对比项 | CC-Switch | SynaRoute | 理由 |
|--------|----------|-----------|------|
| 默认状态 | ✅ 启用 | ❌ 关闭 | SynaRoute 更保守 |
| 用户体验 | 即插即用 | 需手动开启 | 避免意外流量 |
| 隐私保护 | 自动查询 | 显式授权 | 用户更清楚发生了什么 |

### 实现完整性

✅ **SynaRoute 的余额查询实现是完整的**，且已修复 cc-switch 对比时发现的两个问题：
1. User-Agent 缺失导致 403
2. 默认 URL 路径错误

---

## 💡 解决方案

### 方案 A: 用户手动开启（当前设计）

**步骤**:
1. 打开 SynaRoute
2. 进入"密钥管理"
3. 编辑要查询余额的 Key
4. 切换到"余额查询"选项卡
5. 打开"启用余额查询"开关
6. 点击"保存"

**优点**: 
- ✅ 用户明确知道哪些 Key 会查余额
- ✅ 避免意外的 API 调用
- ✅ 隐私保护更好

**缺点**: 
- ❌ 需要额外操作
- ❌ 新用户可能不知道要开启

---

### 方案 B: 修改默认值为启用（建议）

**修改位置**: `src-tauri/src/model.rs:504`

```rust
impl Default for BalanceQuery {
    fn default() -> Self {
        Self {
            enabled: true,  // ✅ 改为 true
            // ... 其他保持不变
        }
    }
}
```

**优点**:
- ✅ 开箱即用，与 cc-switch 体验一致
- ✅ 新用户无需额外配置
- ✅ 导入 cc-switch Key 后立即可查

**缺点**:
- ❌ 可能产生意外的 API 调用
- ❌ 用户不清楚为什么会有额外请求

---

### 方案 C: 智能默认（推荐）⭐

**策略**:
- 手动添加的 Key: `enabled: false` (保守)
- 从 cc-switch 导入的 Key: `enabled: true` (延续习惯)

**实现位置**: `src-tauri/src/ccswitch.rs:391` 的 `import` 函数

```rust
pub fn import(store: &Arc<Store>, source_ids: &[String]) -> AppResult<ImportReport> {
    // ... 现有逻辑 ...
    
    // 导入时设置默认余额查询配置
    let balance_query = Some(BalanceQuery {
        enabled: true,  // ✅ 从 cc-switch 导入的默认启用
        ..Default::default()
    });
    
    // ... 创建 Key 时带上 balance_query ...
}
```

**优点**:
- ✅ 最佳用户体验
- ✅ 兼顾安全和便利
- ✅ 符合用户预期

---

## 📊 对比 CC-Switch 的实现

### CC-Switch 的余额查询实现

**位置**: `usage_script` 功能

```javascript
// cc-switch 内置的通用模板
{
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: { "Authorization": "Bearer {{apiKey}}" }
  },
  extractor: function(response) {
    const remaining = response?.remaining ?? 
                      response?.quota?.remaining ?? 
                      response?.balance;
    const unit = response?.unit ?? 
                 response?.quota?.unit ?? 
                 "USD";
    return { remaining, unit };
  }
}
```

### SynaRoute 的等价实现

**位置**: `balance.rs:32-66`

```rust
// 声明式候选字段链（与 cc-switch 的 `??` 回退等价）
const REMAINING_CANDIDATES: &[&str] = &[
    "remaining",
    "quota.remaining",
    "balance",
    // ... 15+ 个候选
];

const UNIT_CANDIDATES: &[&str] = &[
    "unit",
    "quota.unit",
    "currency",
    // ... 6 个候选
];

// 自动回退逻辑
pub fn extract_balance(body: &Value) -> BalanceResult {
    let remaining = find_in_candidates(body, REMAINING_CANDIDATES);
    let unit = find_in_candidates(body, UNIT_CANDIDATES)
        .unwrap_or("USD".into());  // 默认 USD
    // ...
}
```

### 功能对齐度

| 功能 | CC-Switch | SynaRoute | 状态 |
|------|----------|-----------|------|
| 占位符展开 | ✅ | ✅ | 完全对齐 |
| 字段回退逻辑 | ✅ | ✅ | 完全对齐 |
| 默认启用 | ✅ | ❌ | **差异点** |
| User-Agent | ✅ | ✅ | 已修复 |
| 默认 URL | ✅ | ✅ | 已修复 |
| 认证方式 | ✅ | ✅ | 完全对齐 |
| 错误处理 | ✅ | ✅ | SynaRoute 更详细 |

**结论**: 除了默认启用状态，SynaRoute 的实现**完全对齐** cc-switch，且在某些方面更完善。

---

## 🚀 立即修复建议

### 快速修复（5 分钟）

修改 `src-tauri/src/model.rs:504`:

```rust
enabled: true,  // 从 false 改为 true
```

重新编译并发布 `v0.1.22`。

---

### 完整修复（30 分钟）

实现**方案 C: 智能默认**:

1. 修改 `ccswitch.rs` 导入逻辑，为导入的 Key 启用余额查询
2. 手动添加的 Key 保持 `enabled: false`
3. 添加前端提示："从 cc-switch 导入的 Key 已自动启用余额查询"
4. 更新文档说明

---

## 📝 总结

### 问题根因

❌ **不是实现不完整**  
❌ **不是抄错了 cc-switch**  
✅ **是默认配置的设计选择差异**

### 实现质量

✅ **SynaRoute 的余额查询实现是完整且正确的**  
✅ **已修复 cc-switch 对比时发现的问题**  
✅ **功能完全对齐，部分方面更优**

### 推荐行动

**立即**: 将 `enabled` 默认值改为 `true`，发布 `v0.1.22`  
**后续**: 实现智能默认策略，提升用户体验

---

**需要我立即实施修复吗？** 🔧
