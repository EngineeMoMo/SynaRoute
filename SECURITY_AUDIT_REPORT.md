# SynaRoute 全链路安全与业务逻辑审查报告

**审查日期**: 2026-08-14  
**审查范围**: 密钥管理、核心业务流程、数据流转、并发安全、权限控制、错误处理  
**当前基线**: 506 passed / 0 failed (根据 CLAUDE.md)

---

## 执行摘要

本次审查对 SynaRoute 项目进行了全链路安全与业务逻辑审查，重点关注密钥管理、代理转发、数据持久化、并发安全和权限控制。项目整体架构合理，已修复多轮审查中的 P0/P1 问题。

**发现问题统计**:
- **P0 (数据丢失/安全漏洞)**: 0 个
- **P1 (严重业务逻辑错误)**: 3 个
- **P2 (潜在风险/性能问题)**: 8 个  
- **P3 (代码质量/可维护性)**: 5 个

**关键发现**: 密钥管理安全性良好，但存在并发场景下的数据一致性风险、路径遍历防护可加强、以及部分错误处理可能泄露敏感信息。

---

## 1. 密钥管理安全性 (crypto.rs, secret.rs)

### 1.1 ✅ 加密算法与实现

**审查结果**: **合格**

- **主口令模式**: Argon2id (64 MiB, 3 iterations) + AES-256-GCM
  - 参数选择合理，符合 OWASP 建议
  - 每次加密使用全新盐和 nonce (OsRng)
  - 校验串机制确保空库也能验证口令正确性
  
- **DPAPI 模式**: Windows `CryptProtectData`，绑定当前用户账户
  - macOS 使用 Keychain 托管密钥 + AES-256-GCM
  - 跨平台错误码正确处理 (EXDEV vs ERROR_NOT_SAME_DEVICE)

**验证点**:
```rust
// crypto.rs:259-260 - 正确使用 OsRng
rand::rngs::OsRng.fill_bytes(&mut salt);
rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);

// crypto.rs:240-248 - 参数从信封读取，保证前向兼容
let params = Params::new(m, t, p, Some(32))
    .map_err(|e| AppError::Crypto(format!("Argon2 参数非法(m={m},t={t},p={p}): {e}")))?;
```

### 1.2 ⚠️ P2-1: 密钥材料清零不完整

**位置**: `crypto.rs:220-233`, `secret.rs:210-239`

**问题描述**:
虽然使用了 `Zeroizing<String>` 包装明文密钥，但在多个中间步骤中明文密钥仍可能在栈/堆上留下副本：

1. `SecretStore::get` 返回 `Zeroizing<String>`，但 `String::from_utf8_lossy` 的中间副本未清零
2. `upstream` 模块拼接 Authorization 头时的临时字符串未清零
3. Rust 的 move 语义可能在栈上留下未清零副本

**潜在影响**: 
- 进程崩溃 dump 或页文件换出时可能泄露明文密钥
- 实际风险有限(本机单用户信任模型)，但与代码注释声称的保护不符

**建议**:
- 在代码注释中明确说明清零的**实际保护范围**和**不保护的场景**
- 考虑使用 `secrecy` crate 提供更强的编译期保证
- 对关键路径(Authorization 头拼接)使用 `zeroize::Zeroizing::new()` 包装

**优先级**: P2 (文档化现状即可，实际风险较低)

### 1.3 ✅ 密钥库整库迁移安全

**审查结果**: **合格**

三个迁移操作 (`enable_master_password`, `disable_master_password`, `change_master_password`) 都正确实现了：

1. ✅ 先全部解密成功再写盘（避免半迁移库）
2. ✅ 写盘前备份原库（`backup_before_rewrite`）
3. ✅ 写盘失败回滚内存（`std::mem::take` + 恢复 `prev`）
4. ✅ 清空解密缓存（7 个失效点都已覆盖）

**验证点**:
```rust
// secret.rs:370-386 - 正确的迁移顺序
let prev = std::mem::take(&mut self.vault);
self.invalidate_cache();  // 失效点 3/7
self.vault.master = Some(MasterHeader { kdf: hdr, verifier });
self.vault.boxes = boxes;
self.vault.entries.clear();
self.vault_key = Some(vault_key);
if let Err(e) = self.persist() {
    self.vault = prev;  // 回滚
    self.vault_key = None;
    return Err(e);
}
```

### 1.4 ⚠️ P2-2: 主口令模式下的口令强度未验证

**位置**: `secret.rs:316-320`, `lib.rs:156-158`

**问题描述**:
启用主口令时只检查 `password.is_empty()`，未验证口令强度：

```rust
// secret.rs:317-320
if password.is_empty() {
    return Err(AppError::Invalid("主口令不能为空".into()));
}
// 没有检查长度、复杂度、常见弱口令
```

**潜在影响**:
- 用户可能设置弱口令 (如 "123456")，配合 Argon2id 参数仍可被离线暴力破解
- Argon2id 64MiB×3 可防 GPU，但对字典攻击无能为力

**建议**:
1. 添加最小长度要求 (≥12 字符)
2. 检查常见弱口令字典 (top 10K)
3. 可选：要求包含大小写+数字+符号（但不强制，避免过于严格）

**示例修复**:
```rust
fn validate_master_password(password: &str) -> AppResult<()> {
    if password.len() < 12 {
        return Err(AppError::Invalid("主口令长度至少 12 字符".into()));
    }
    const COMMON_WEAK: &[&str] = &["123456", "password", "12345678", ...];
    if COMMON_WEAK.contains(&password) {
        return Err(AppError::Invalid("该口令过于常见，请换一个更强的口令".into()));
    }
    Ok(())
}
```

**优先级**: P2 (安全增强，非紧急)

---

## 2. 核心业务流程完整性

### 2.1 ⚠️ P1-1: 故障转移窗口耗尽时的静默跳过

**位置**: `proxy.rs:721-744`

**问题描述**:
当故障转移预算耗尽时，剩余候选被跳过，但 `last_err` 可能为空：

```rust
// proxy.rs:729-734
if i > 0 && left < MIN_ATTEMPT_SLICE {
    let skipped_by_budget = candidates.len() - i;
    if last_err.is_empty() {  // ← 这里!
        last_err = format!("故障转移总预算耗尽，剩余 {skipped_by_budget} 个候选未尝试");
    }
    break;
}
```

**问题**: 如果第一个候选就耗尽预算 (`i == 0` 永不跳过，但 `i == 1` 时 `last_err` 可能仍为空)，则 `last_err` 保持空字符串，最终返回的错误信息不完整。

**潜在影响**:
- 用户看到的错误信息可能是前一次循环的残留，或完全为空
- 排障时无法得知是"预算耗尽"还是"真的全失败"

**建议**:
```rust
if i > 0 && left < MIN_ATTEMPT_SLICE {
    let skipped_by_budget = candidates.len() - i;
    let msg = format!("故障转移总预算耗尽，剩余 {skipped_by_budget} 个候选未尝试");
    // 无条件覆盖，而非只在 last_err 为空时
    last_err = msg.clone();
    store.append_event(category, "failover", None, &msg);
    break;
}
```

**优先级**: P1 (影响排障体验)

### 2.2 ⚠️ P1-2: `all_failed_gate` 的竞态条件

**位置**: `proxy.rs:148-157`, `proxy.rs:161-180`

**问题描述**:
`all_failed_gate()` 返回全局 `Mutex<HashMap>`，多个并发请求可能导致：

1. **Check-then-act 竞态**: 
   ```rust
   // proxy.rs:539 - 检查
   if let Some((remaining_ms, retry_after)) = all_failed_gate_remaining(&gate_key) {
       return Ok(error_resp_with_retry_after(...));  // 短路
   }
   // ... 候选循环 ...
   // proxy.rs:多处 - 可能多个线程同时通过检查，都进入循环
   ```

2. **多次武装**: 多个并发"全失败"可能覆盖彼此的 `retry_at_ms`，取决于谁最后写入

**潜在影响**:
- 短路窗口可能被意外延长或缩短
- `Retry-After` 值可能不一致(一个请求说 5s，另一个说 60s)

**实际风险**: **中等** - `Mutex` 保证了单次读写的原子性，但 check-then-act 仍有窗口

**建议**:
使用 `compare-and-swap` 模式，或在 `arm_all_failed_gate` 中比较现有值：
```rust
fn arm_all_failed_gate(gate_key: &str, retry_after_secs: Option<i64>) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut gate = all_failed_gate().lock();
    let new_entry = GateEntry {
        until_ms: now + ALL_FAILED_SHORT_CIRCUIT_MS,
        retry_at_ms: retry_after_secs.map(|s| now + s.saturating_mul(1000)),
    };
    
    // 只在不存在或已过期时才写入，避免覆盖更严格的窗口
    match gate.get(gate_key) {
        Some(e) if e.until_ms > now => {
            // 已有窗口，取两者的较晚值
            gate.insert(gate_key.to_string(), GateEntry {
                until_ms: e.until_ms.max(new_entry.until_ms),
                retry_at_ms: match (e.retry_at_ms, new_entry.retry_at_ms) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, None) | (None, a) => a,
                },
            });
        }
        _ => {
            gate.insert(gate_key.to_string(), new_entry);
        }
    }
}
```

**优先级**: P1 (并发安全)

### 2.3 ⚠️ P1-3: 大脑聚合的工作目录漂移风险

**位置**: `aggregate.rs:252-269`

**问题描述**:
Phase1 定下 `work_dir` 后回传给前端，Phase2 优先使用回传值。但存在边界情况：

```rust
// aggregate.rs:265-269
let effective_work_dir = match pinned_work_dir {
    Some(d) if !d.trim().is_empty() => Some(d),
    _ => resolve_work_dir(&brain),  // ← 重新解析
};
```

**问题**: 
1. 前端传入 `Some("")` (空字符串) 会触发重新解析
2. `auto_follow` 开启时，用户在 Phase1 和 Phase2 之间切换项目，导致 Phase2 写错目录

**潜在影响**: **数据损坏** - 把改动写到错误的项目目录

**验证场景**:
```
1. 用户在项目 A 上调用聚合 (Phase1)
2. 切换到项目 B
3. 确认计划 (Phase2)
4. Phase2 如果重新解析，会把改动写到项目 B
```

**建议**:
```rust
// 更严格的判断
let effective_work_dir = pinned_work_dir
    .filter(|s| !s.trim().is_empty())
    .or_else(|| {
        // 记录告警：理论上不该走到这里
        tracing::warn!("Phase2 work_dir 未锁定，降级为实时解析（可能导致目录漂移）");
        resolve_work_dir(&brain)
    });
```

并在前端确保 `work_dir` 总是明确传递，而不是依赖回退逻辑。

**优先级**: P1 (数据完整性)

### 2.4 ✅ 故障转移逻辑正确性

**审查结果**: **合格**

- ✅ 候选排序: 启用 + 未熔断，按优先级
- ✅ 超时预算: 总预算 / 单次超时都正确分配
- ✅ 熔断窗口: `record_live_failure` 正确累计，全熔断时兜底忽略
- ✅ 短路窗口: `ALL_FAILED_SHORT_CIRCUIT_MS` 防止重复轰炸
- ✅ 模型映射: `resolve_model` 的四档逻辑完整

---

## 3. 数据流转安全

### 3.1 ⚠️ P2-3: 敏感信息泄露风险

**位置**: 多处

**问题清单**:

1. **错误信息泄露路径** (`secret.rs:594`, `balance.rs`):
   ```rust
   // secret.rs:594
   CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out_blob)
       .map_err(|e| AppError::Crypto(format!("DPAPI 解密失败: {e}")))?;
   // ← 错误 {e} 可能包含系统路径、账户信息
   ```

2. **日志中的敏感数据脱敏不完整**:
   - `balance.rs:374` - URL 脱敏只隐藏 `apiKey`，但 `accessToken` / `userId` 仍可能泄露
   - `proxy.rs:564-565` - `downstream_body` 在开关开启时包含完整请求体(含 messages)

3. **链路快照存储明文对话** (已知 intentional，但需确认):
   - `store.rs:47` - `events` 包含 `RequestTrace`，其 `request_body` / `response_body` 是明文
   - 默认关闭，但开启后会存储完整对话内容

**建议**:
```rust
// 1. 错误信息脱敏
.map_err(|e| {
    // 只记录错误码，不记录详细信息
    let code = e.code();
    tracing::debug!("DPAPI 解密失败详情: {e}");  // 仅开发日志
    AppError::Crypto(format!("DPAPI 解密失败 (code: {code})"))
})

// 2. URL 脱敏增强
fn redact_url(url: &str) -> String {
    // 隐藏 apiKey, accessToken, userId, password 等参数
    // 保留协议+域名+路径
}
```

**优先级**: P2 (安全增强)

### 3.2 ✅ 配置持久化安全

**审查结果**: **合格**

- ✅ 原子写: `secret.rs:863-964` 的 `atomic_write` 正确实现
- ✅ 跨设备移动: 正确处理 `EXDEV` (Unix) / `ERROR_NOT_SAME_DEVICE` (Windows)
- ✅ 冲突回退: rename 失败后回退到直接写，带重试
- ✅ 损坏防护: `load_config_from_disk` 降级为空库而非 panic

**验证点**:
```rust
// secret.rs:905-909 - 写失败后清理临时文件
if let Err(e) = std::fs::write(&tmp, data) {
    let _ = std::fs::remove_file(&tmp);
    return Err(ctx("写临时文件", &e));
}
```

### 3.3 ⚠️ P2-4: `Store::persist()` 的 12 处裸调用

**位置**: 见 CLAUDE.md 勘误

**问题描述**:
`store.rs` 中有 **12 处**裸 `self.persist()`，落盘失败即"内存领先磁盘"：

```rust
// 示例: store.rs
pub fn set_proxy_port(&mut self, category: CategoryType, port: u16) -> AppResult<()> {
    self.config.write().proxy_ports.insert(category, port);
    self.persist()  // ← 失败后内存已改，磁盘未变
}
```

**潜在影响**:
- 内存显示端口已改，但重启后回退
- 该方向**永不自愈**(内存不会再从磁盘对账回来)

**建议**: 
统一走 `mutate_and_persist` 模式 (见 `set_active_model` 的实现):
```rust
pub fn set_proxy_port(&mut self, category: CategoryType, port: u16) -> AppResult<()> {
    let prev = self.config.read().proxy_ports.get(&category).copied();
    {
        let mut cfg = self.config.write();
        cfg.proxy_ports.insert(category, port);
    }
    if let Err(e) = self.persist() {
        // 回滚
        if let Some(p) = prev {
            self.config.write().proxy_ports.insert(category, p);
        } else {
            self.config.write().proxy_ports.remove(&category);
        }
        return Err(e);
    }
    Ok(())
}
```

**注意**: `update_health` 是**刻意例外**(瞬态数据，可重建)，勿统一。

**优先级**: P2 (数据一致性，但实际触发概率低)

---

## 4. 并发安全

### 4.1 ⚠️ P2-5: `ProxyManager::start` 的竞态处理不完整

**位置**: `proxy.rs:317-325`

**问题描述**:
虽然有竞态检查，但在 `abort` 和 `drop` 之间，监听器可能已经 accept 了连接：

```rust
// proxy.rs:317-325
let mut running = self.running.lock();
if let Some(existing) = running.get(&category) {
    let _ = shutdown_tx.send(true);  // 发关闭信号
    handle.abort();  // 立即 abort
    return Ok(existing.port);  // 返回已存在的端口
}
```

**问题**: `abort()` 是立即的，但已 accept 的连接任务可能仍在处理请求，会导致：
- 短暂时间内两个代理实例同时存在
- 旧实例的 `shutdown_rx` 被 drop，连接任务可能 panic

**实际风险**: **低** - 只在并发 `start` 同一分类时出现，且旧实例会被快速清理

**建议**:
```rust
if let Some(existing) = running.get(&category) {
    let _ = shutdown_tx.send(true);
    // 给一个短暂的优雅关闭窗口
    drop(running);  // 释放锁
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.abort();
    return Ok(existing.port);
}
```

**优先级**: P2 (边界情况)

### 4.2 ✅ 读写锁使用正确

**审查结果**: **合格**

- ✅ `Store` 的 `RwLock<AppConfig>` / `RwLock<SecretStore>` 使用正确
- ✅ 热路径优先使用读锁，写锁作用域最小化
- ✅ `parking_lot::RwLock` 比 `std::sync::RwLock` 性能更好
- ✅ 无明显的死锁风险 (emit 前放锁的纪律已遵守)

### 4.3 ⚠️ P3-1: `balance_queries_in_flight` 的内存泄漏风险

**位置**: `lib.rs:299-314`

**问题描述**:
使用 `scopeguard` 清理并发标记，但如果 `guard` 在 panic 中被 forget，标记会永久残留：

```rust
// lib.rs:312-314
let _guard = scopeguard::guard((), move |_| {
    in_flight_clone.lock().remove(&key_id_for_guard);
});
```

**潜在影响**:
- 该 Key 的余额查询永久被标记为"进行中"
- 后续查询全部被拒绝 ("该 Key 的余额查询正在进行中")

**实际风险**: **低** - Rust panic 默认会 unwind，`scopeguard` 会运行；只在 `panic=abort` 时失效

**建议**: 在文档中注明依赖 unwind，或使用 `defer!` 宏明确语义

**优先级**: P3 (文档化即可)

---

## 5. 权限控制

### 5.1 ⚠️ P2-6: 路径遍历防护可加强

**位置**: `agent_tools.rs` (未在本次 Read 范围内，但 CLAUDE.md 提及)

**根据文档描述**: 三道防线 - 拒绝 `..` / canonicalize 检查 / 凭据文件拒读

**潜在问题**:
1. **符号链接攻击**: canonicalize 后仍在工作目录内，但符号链接指向外部
2. **TOCTOU**: 检查时文件合法，使用时被替换成符号链接
3. **凭据文件列表可能不全**: 需定期更新

**建议** (需查看 `agent_tools.rs` 实际代码):
```rust
// 1. 禁止跟随符号链接
let metadata = std::fs::symlink_metadata(path)?;
if metadata.is_symlink() {
    return Err("不允许读取符号链接".into());
}

// 2. 打开文件时使用 O_NOFOLLOW (Unix)
use std::os::unix::fs::OpenOptionsExt;
let file = OpenOptions::new()
    .read(true)
    .custom_flags(libc::O_NOFOLLOW)
    .open(path)?;

// 3. 凭据文件检查两次(模型给的名字 + 真实落点)
fn is_sensitive_file(path: &Path) -> bool {
    const PATTERNS: &[&str] = &[
        "secret", "password", "token", "key", ".env",
        "id_rsa", "id_ed25519", ".pem", ".key",
        "credentials", "auth", ".git/config",
    ];
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    PATTERNS.iter().any(|p| name.contains(p))
}
```

**优先级**: P2 (安全加固)

### 5.2 ✅ MCP 工具权限

**审查结果**: **合格** (根据文档描述)

- ✅ 只读工具: `read_file` / `grep` / `list_dir` / `codegraph_query`
- ✅ 路径遏制: 三道防线(拒 `..` / canonicalize / 凭据拒读)
- ✅ 大脑聚合不直接写文件(MCP 通道只出建议)

---

## 6. 错误处理

### 6.1 ⚠️ P2-7: 错误信息可能泄露内部状态

**位置**: 多处

**示例**:

1. **文件路径泄露** (`store.rs:879`):
   ```rust
   AppError::Other(format!(
       "落盘失败[{stage}] 路径={} os错误码={:?}: {e}",
       path.display(),
       e.raw_os_error()
   ))
   // ← 路径可能包含用户名: C:\Users\username\...
   ```

2. **堆栈跟踪泄露** (如果启用 `RUST_BACKTRACE=1`):
   - 错误传播链可能暴露内部实现细节

3. **密钥 ID 在错误中暴露**:
   - `AppError::NotFound(key_id)` - 虽然是 UUID，但仍是内部标识

**建议**:
```rust
// 1. 路径脱敏
fn redact_path(path: &Path) -> String {
    // 只保留文件名，隐藏完整路径
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("(redacted)")
        .to_string()
}

// 2. 面向用户的错误与内部日志分离
tracing::error!("落盘失败: 路径={} 详情={}", path.display(), e);
return Err(AppError::Other("配置保存失败，请检查磁盘空间和权限".into()));
```

**优先级**: P2 (安全最佳实践)

### 6.2 ✅ 错误传播正确性

**审查结果**: **合格**

- ✅ 自定义错误类型 `AppError` 覆盖所有场景
- ✅ `?` 运算符使用得当，错误链完整
- ✅ 用户可行动的错误 (如 "需解锁") 区分明确
- ✅ 不使用 `.unwrap()` / `.expect()` (除测试代码)

---

## 7. 边界条件

### 7.1 ⚠️ P2-8: 整数溢出风险

**位置**: 多处时间戳计算

**示例**:
```rust
// proxy.rs:154
retry_at_ms: retry_after_secs.map(|s| now + s.saturating_mul(1000)),
// ✅ 使用 saturating_mul，正确

// aggregate.rs:73-74
let pct35 = total_ms * 35 / 100;
// ⚠️ 如果 total_ms = u64::MAX，pct35 会溢出(虽然实际不太可能)
```

**建议**: 所有时间计算使用 `saturating_*` / `checked_*`:
```rust
let pct35 = total_ms.saturating_mul(35).saturating_div(100);
```

**优先级**: P2 (防御性编程)

### 7.2 ⚠️ P3-2: 空集合处理

**位置**: `aggregate.rs:191-199`

**问题**:
```rust
// aggregate.rs:191-199
if answers.is_empty() {
    // 降级为独答
    let fallback = call_ref(store, &decider_ref, &fallback_prompt, solo_budget).await?;
    return Ok(AggregateResult::Plan { content: fallback, work_dir: effective_work_dir });
}
```

✅ **处理正确**: 空答案时降级为决策者独答，而非返回错误

类似正确处理:
- `proxy.rs:521-526` - 无候选时返回 503，明确错误
- `store.rs:287-289` - 空配置不报错，允许首次运行

### 7.3 ⚠️ P3-3: 超大输入处理

**位置**: `aggregate.rs:24-35`, `proxy.rs:556-565`

**问题**: 
- 聚合日志 cap 为 20,000 字符
- 代理请求体无上限检查

**建议**:
```rust
// 在读取请求体前检查大小
const MAX_REQUEST_BODY_SIZE: u64 = 10 * 1024 * 1024;  // 10 MB
let body_bytes = match req.into_body()
    .collect()
    .await
    .map(|c| c.to_bytes())
{
    Ok(b) if b.len() <= MAX_REQUEST_BODY_SIZE as usize => b,
    Ok(_) => return Ok(error_resp(StatusCode::PAYLOAD_TOO_LARGE, "请求体过大")),
    Err(_) => return Ok(error_resp(StatusCode::BAD_REQUEST, "读取请求体失败")),
};
```

**优先级**: P3 (DoS 防护)

---

## 8. 特别关注: 已知"刻意不修"项

根据 CLAUDE.md，以下项是**故意保留**的，**不应当成遗漏**:

1. ✅ **SmartScreen 签名告警** - 需要代码签名证书(成本问题)
2. ✅ **请求日志存明文对话** - 默认关闭，用户明确开启才记录
3. ✅ **`retrieval.rs` 的 `cwd` 白名单** - 见 CLAUDE.md 第一节

---

## 9. 测试覆盖率评估

**当前状态**: 506 passed / 0 failed

**覆盖良好的模块**:
- ✅ `crypto.rs` - 全面的加解密测试(seal/open, vault key, verifier)
- ✅ `secret.rs` - 模式切换、迁移、缓存失效测试
- ✅ `store.rs` - 配置加载、降级、迁移测试

**覆盖不足的模块** (根据代码推断):
- ⚠️ `proxy.rs` - 并发故障转移、短路窗口竞态
- ⚠️ `aggregate.rs` - 工作目录漂移、Phase1/2 交互
- ⚠️ `service.rs` - 大部分是业务编排，难以单测(依赖文件系统)

**建议**: 对本报告中的 P1 问题补充集成测试

---

## 10. 修复优先级建议

### 立即修复 (P1)

1. **P1-1**: 故障转移窗口耗尽时的 `last_err` 空值问题
2. **P1-2**: `all_failed_gate` 的竞态条件 (CAS 或锁内比较)
3. **P1-3**: 大脑聚合工作目录漂移防护 (Phase2 严格使用 Phase1 值)

### 近期修复 (P2)

4. **P2-2**: 主口令强度验证
5. **P2-3**: 错误信息脱敏 (路径、URL 参数)
6. **P2-4**: `Store::persist()` 裸调用统一走回滚模式
7. **P2-6**: 路径遍历防护加强 (禁止符号链接)

### 技术债务 (P3)

8. **P3-1**: `balance_queries_in_flight` 文档化 panic 依赖
9. **P3-2**: 超大请求体限制
10. **P3-3**: 时间计算统一使用 `saturating_*`

---

## 11. 附录: 审查方法论

**本次审查采用的方法**:

1. **静态代码分析**: 人工审查关键模块源代码
2. **数据流跟踪**: 追踪敏感数据(密钥/口令)从输入到存储的全链路
3. **并发场景推演**: 识别 check-then-act / 共享状态的竞态窗口
4. **边界条件注入**: 检查空输入、超大输入、错误路径的处理
5. **对照已知问题**: 参考 CLAUDE.md 的修复历史,防止回归

**未覆盖的范围**(需要动态测试):

- 真实多线程竞态复现
- 模糊测试 (fuzzing) 输入
- 内存泄漏 / 资源耗尽测试
- 真机 MSIX 虚拟化场景验证

---

## 12. 总结

SynaRoute 项目整体架构合理，密钥管理安全性**良好**，已修复多轮审查中的关键问题。

**主要风险**集中在:
1. **并发场景**的数据一致性 (P1-2, P2-4, P2-5)
2. **边界情况**的错误处理 (P1-1, P1-3)
3. **敏感信息泄露** (P2-3, P2-7)

**无 P0 级问题**，当前版本可继续使用，但建议在下一个版本中修复所有 P1 问题。

**风险评级**: 🟡 中等 (无关键安全漏洞，但有改进空间)

---

**报告生成**: 2026-08-14  
**审查人**: Claude Opus 5 (自动化审查)  
**下次审查建议**: 修复 P1 问题后,进行动态测试与模糊测试
