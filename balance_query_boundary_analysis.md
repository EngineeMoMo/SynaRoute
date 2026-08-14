# 余额查询边界条件与异常输入审查报告

## 审查目标

检查 `query_key_balance` 及相关路径在以下边界条件下的行为：
1. 未配置余额查询的 Key
2. 主口令锁定时查询
3. Key 被删除后正在进行的查询
4. 空字符串或不存在的 key_id
5. api_key_ref 指向不存在的密钥
6. 上游返回超大响应（> 1MB）
7. 缓存过期判断的边界值
8. balance_query.url 为空或格式错误

---

## 1. 未配置余额查询的 Key 调用 query_key_balance

**代码位置**: `src-tauri/src/lib.rs:310-319`

```rust
let Some(cfg) = key.balance_query.clone() else {
    let reason = "该 Key 未配置余额查询";
    state.store.append_event(
        key.category_id,
        "余额",
        Some(&key_id),
        &format!("查询失败：{}", reason),
    );
    return Ok(crate::model::BalanceResult::failed(reason));
};
```

**行为**: ✅ **正确处理**
- 返回 `BalanceResult::failed("该 Key 未配置余额查询")`
- 记录到运行日志
- **不返回 `Err`**，符合设计意图（"余额查不到是常态"）
- 前端可正常在卡片上显示原因

---

## 2. 主口令锁定时查询的行为

**代码位置**: `src-tauri/src/lib.rs:322-331`

```rust
if state.store.secrets.read().is_locked() {
    let reason = "密钥库已锁定，请先用主口令解锁";
    state.store.append_event(
        key.category_id,
        "余额",
        Some(&key_id),
        &format!("查询失败：{}", reason),
    );
    return Ok(crate::model::BalanceResult::failed(reason));
}
```

**行为**: ✅ **正确拒绝**
- 提示明确："密钥库已锁定，请先用主口令解锁"
- 不是含糊的"余额接口坏了"
- 记录到日志
- **在取密钥之前检查**，避免后续 `secrets.read().get()` 返回 `None` 时原因不清晰

---

## 3. Key 被删除后，正在进行的查询如何处理

**代码位置**: `src-tauri/src/lib.rs:307-309`

```rust
let Some(key) = state.store.get_key(&key_id) else {
    return Err(crate::error::AppError::NotFound(key_id));
};
```

**行为**: ✅ **正确处理**
- 在并发控制**之后**、网络请求**之前**检查 Key 是否存在
- 返回 `Err(NotFound)` —— 这是"调用方用错了"的情况，符合文档说明（`:272`）
- **在 scopeguard 保护范围内**（`:301-305`），`in_flight` 标记会被正确清除
- **时序正确**：即使 Key 在查询进行中被删除，`get_key` 返回 `None` → 返回 `NotFound`

**可能的问题**: ⚠️ **潜在的小竞态**
- 如果删除发生在 `:307` **之前**，查询正常失败 → ✅
- 如果删除发生在 `:307` **之后**但请求尚未完成，查询会继续进行并尝试写缓存
- **写缓存时** (`update_balance_cache`, `store.rs:1922-1931`)：
  ```rust
  if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
      k.cached_balance = Some(result);
      drop(cfg);
      self.persist()?;  // ← 找不到 Key 时不会执行到这里
  }
  Ok(())  // ← 静默成功（Key 已删除时不写缓存）
  ```
- **结论**: 静默成功，不崩溃。缓存更新失败不影响查询结果返回。✅ **无实质问题**

---

## 4. 空字符串 key_id 或不存在的 key_id 的处理

### 4.1 空字符串 key_id

**行为**: ✅ **正确处理**
- `store.get_key("")` 返回 `None`（`:307` 的 `find` 不会命中）
- 返回 `Err(AppError::NotFound(""))`
- 前端会看到异常，而不是静默失败

### 4.2 不存在的 key_id

**行为**: ✅ **同上，返回 `NotFound` 错误**

**并发控制的清理**: ✅ **正确**
- scopeguard (`:301-305`) 确保无论如何都会从 `balance_queries_in_flight` 移除
- 即使返回 `Err`，标记也会被清除

---

## 5. api_key_ref 指向不存在的密钥时

**代码位置**: `src-tauri/src/lib.rs:334-345`

```rust
let secret_key = cfg.api_key_ref.as_deref().unwrap_or(&key_id);
let secret = state.store.secrets.read().get(secret_key).ok().flatten();
let Some(secret) = secret else {
    let reason = "未配置密钥";
    state.store.append_event(
        key.category_id,
        "余额",
        Some(&key_id),
        &format!("查询失败：{}", reason),
    );
    return Ok(crate::model::BalanceResult::failed(reason));
};
```

**行为**: ✅ **正确处理**
- `api_key_ref` 指向的密钥不存在 → `get(secret_key)` 返回 `Ok(None)` → `flatten()` 后是 `None`
- 返回 `BalanceResult::failed("未配置密钥")`
- **不会 panic**，不会崩溃
- 记录到日志

**细节正确性**:
- `get()` 返回 `Result<Option<Zeroizing<String>>>`（`secret.rs`）
- `.ok()` 把 `Err`（如主口令锁定）变 `None`
- `.flatten()` 把 `Ok(None)` 也变 `None`
- **主口令锁定的判定已在 `:322` 先做**，这里的 `ok()` 主要是处理其它可能的 `Err`

---

## 6. 上游返回超大响应（> 1MB）是否会 OOM

**代码位置**: `src-tauri/src/balance.rs:343-347`

```rust
let text = match resp.text().await {
    Ok(t) => t,
    Err(e) => return BalanceResult::failed(format!("读取响应失败: {e}")),
};
```

**分析**: ⚠️ **存在 OOM 风险，但实际风险低**

1. **reqwest 默认无大小限制**：
   - `resp.text()` 会把整个响应读入内存
   - 理论上，恶意上游返回 GB 级响应会导致 OOM

2. **缓解因素**：
   - **超时保护**: `timeout_secs`（默认 10 秒）会限制传输时间
   - **实际场景**: 余额接口通常返回 < 1KB 的 JSON
   - **上游是用户自己配置的**，不是公开攻击面

3. **对比转发路径**：
   - 转发路径对请求体有大小限制（见 `proxy.rs`）
   - 但余额查询没有显式的响应体大小限制

**建议**: P3 优化机会
- 可添加响应体大小限制（如 1MB）
- 但余额查询不是高频路径，且超时已提供基本保护
- **当前不算缺陷**，建议记入 P3 优化清单

---

## 7. 缓存过期判断的边界值

**代码位置**: `src-tauri/src/store.rs:1940-1964`

```rust
pub fn get_balance_cache(&self, key_id: &str) -> Option<BalanceResult> {
    let cfg = self.config.read();
    let key = cfg.keys.iter().find(|k| k.id == key_id)?;
    let cached = key.cached_balance.as_ref()?;

    let cache_duration_secs = if let Some(bq) = &key.balance_query {
        if bq.auto_interval_min > 0 {
            (bq.auto_interval_min as i64) * 60
        } else {
            5 * 60  // 默认 5 分钟
        }
    } else {
        5 * 60
    };

    let now = chrono::Utc::now().timestamp_millis();
    let age_secs = (now - cached.queried_at) / 1000;
    if age_secs < cache_duration_secs {
        Some(cached.clone())
    } else {
        None
    }
}
```

### 7.1 queried_at = 0 的情况

**行为**: ✅ **正确处理**
- `queried_at = 0` 对应 `1970-01-01 00:00:00 UTC`
- `age_secs = (now - 0) / 1000` 会是一个非常大的正数（约 57 年）
- `age_secs < cache_duration_secs` 为 `false` → 返回 `None`（缓存过期）
- **不会 panic**，不会返回陈旧数据

### 7.2 queried_at 是未来时间戳

**行为**: ⚠️ **存在小问题，但实际影响有限**

假设 `queried_at = now + 1 小时`：
- `age_secs = (now - future) / 1000` = 负数（如 -3600）
- `age_secs < cache_duration_secs` → **true**（负数 < 正数）
- 返回 `Some(cached)`，即**未来的缓存被当作有效**

**根源**: 缺少时间戳合法性检查

**实际影响**: ✅ **极低**
- `queried_at` 由 `chrono::Utc::now().timestamp_millis()` 生成（`balance.rs:205`）
- 除非系统时钟严重错误或磁盘数据被人为篡改，否则不会出现未来时间戳
- 即使出现，最坏情况是该 Key 的余额缓存在接下来几小时内不会刷新

**建议**: P3 加固
- 可添加 `if age_secs < 0 { return None; }` 防御未来时间戳
- 但这不是安全问题，只是边界健壮性

### 7.3 整数溢出

**行为**: ✅ **不会溢出**
- `now` 和 `queried_at` 都是 `i64`
- `i64::MAX` 对应 `292,277,026,596` 年（2.9 亿年）
- 即使 `now - queried_at` 的差值，在可预见的未来也不会溢出
- 除以 1000 后更不可能溢出

---

## 8. balance_query.url 为空或格式错误时

**代码位置**: `src-tauri/src/balance.rs:268-271`

```rust
if cfg.url.trim().is_empty() {
    return BalanceResult::failed("未配置查询地址");
}
```

**行为**: ✅ **正确处理**
- 空字符串或纯空白 → 返回 `failed("未配置查询地址")`
- **在网络请求之前检查**，不会浪费资源

**格式错误检查**: `balance.rs:286-288`

```rust
if !url.starts_with("http://") && !url.starts_with("https://") {
    return BalanceResult::failed(format!("查询地址不是合法的 http(s) URL: {url}"));
}
```

**行为**: ✅ **正确处理**
- 占位符展开后仍不是合法 URL → 早报错
- 错误信息包含实际 URL，方便排查

**边界情况**:
1. `url = "{{baseUrl}}/balance"` 但 `baseUrl` 为空 → 展开后 `"/balance"`，不含 `http` → 被拒 ✅
2. `url = "ftp://..."`  → 不是 `http(s)` → 被拒 ✅
3. `url = "http://"` → 通过检查，但 reqwest 会失败 → 报 "请求失败: ..." ✅

---

## 额外发现：缓存写入失败的静默

**代码位置**: `src-tauri/src/lib.rs:373`

```rust
state.store.update_balance_cache(&key_id, result.clone())?;
```

这里 `update_balance_cache` 返回 `AppResult<()>`，用 `?` 传播。

**查看 `update_balance_cache` 实现** (`store.rs:1922-1931`):

```rust
pub fn update_balance_cache(&self, key_id: &str, result: BalanceResult) -> AppResult<()> {
    let mut cfg = self.config.write();
    if let Some(k) = cfg.keys.iter_mut().find(|k| k.id == key_id) {
        k.cached_balance = Some(result);
        drop(cfg);
        self.persist()?;  // ← 可能失败
    }
    Ok(())
}
```

**行为分析**:
- 如果 `persist()` 失败（磁盘满、权限等），`query_key_balance` 会返回 `Err`
- **查询成功但缓存写入失败 → 整个命令报错**
- 前端会看到异常，而查询结果（已拿到）不会返回

**问题**: ⚠️ **P2 - 缓存写入失败不应阻止查询结果返回**

**影响**:
- 用户点刷新 → 查询成功 → 但因磁盘满写缓存失败 → 前端报错
- 用户会以为"余额查询失败"，而实际是"写缓存失败"
- **查询结果白费**，下次点击又要重查

**建议修复**:
```rust
// 尝试更新缓存，失败只记日志，不影响查询结果返回
if let Err(e) = state.store.update_balance_cache(&key_id, result.clone()) {
    tracing::warn!("余额缓存写入失败（不影响本次查询）: {e}");
}

Ok(result)
```

---

## 注意：cached_balance 的 #[serde(skip)]

**代码位置**: `src-tauri/src/model.rs:431`

```rust
#[serde(skip)]
pub cached_balance: Option<BalanceResult>,
```

**与 `update_balance_cache` 的矛盾**: ⚠️ **实现与注释不一致**

- **注释说** (`:428-430`): "只在内存中维护、不落盘"
- **但实际代码** (`store.rs:1928`): `self.persist()?;` —— 会写盘

**真相**:
- `#[serde(skip)]` 确保 `cached_balance` **序列化时被跳过**
- 所以 `persist()` 虽然调用了，但 `cached_balance` **确实不会写进 `config.json`**
- **注释是对的，实现也是对的** ✅

**小建议**: 
- `update_balance_cache` 的注释 (`:1920-1921`) 说"纯内存操作、不落盘"，但代码里调了 `persist()`
- 建议注释改为: "更新内存缓存。`persist()` 会被调用但 `cached_balance` 字段因 `#[serde(skip)]` 不会写盘，重启后缓存自然清空。"

---

## 总结

| 条件 | 行为 | 评级 | 备注 |
|------|------|------|------|
| 1. 未配置余额查询 | ✅ 返回 `failed("该 Key 未配置余额查询")` | 正确 | 符合设计 |
| 2. 主口令锁定 | ✅ 返回 `failed("密钥库已锁定...")` | 正确 | 提示明确 |
| 3. Key 被删除 | ✅ 返回 `Err(NotFound)` | 正确 | scopeguard 保证清理 |
| 4. 空/不存在 key_id | ✅ 返回 `Err(NotFound)` | 正确 | 不静默失败 |
| 5. api_key_ref 不存在 | ✅ 返回 `failed("未配置密钥")` | 正确 | 不崩溃 |
| 6. 超大响应 | ⚠️ 无大小限制，理论上可 OOM | P3 | 超时保护缓解，实际风险低 |
| 7. queried_at = 0 | ✅ 判为过期，返回 `None` | 正确 | 不返回陈旧数据 |
| 7. queried_at 未来 | ⚠️ 被当作有效缓存 | P3 | 实际不会发生，可加固 |
| 8. url 为空 | ✅ 返回 `failed("未配置查询地址")` | 正确 | 早检查 |
| 8. url 格式错误 | ✅ 返回 `failed("不是合法的 http(s) URL...")` | 正确 | 早检查 |
| **额外**: 缓存写入失败 | ⚠️ **阻止查询结果返回** | **P2** | **应记日志但不阻止返回** |

---

## 推荐修复

### P2: 缓存写入失败不应阻止查询结果返回

**文件**: `src-tauri/src/lib.rs:373`

**当前**:
```rust
state.store.update_balance_cache(&key_id, result.clone())?;
Ok(result)
```

**修改为**:
```rust
// 尝试更新缓存，失败只记日志，不影响查询结果返回
if let Err(e) = state.store.update_balance_cache(&key_id, result.clone()) {
    tracing::warn!("余额缓存写入失败（key={}, 不影响本次查询）: {e}", key_id);
}
Ok(result)
```

**理由**:
- 缓存是优化手段，不是查询的核心目标
- 磁盘满/权限问题不应让"已经查到的余额"无法返回给用户
- 失败记日志，供排障时追溯

---

## 其它观察（无需修复）

1. ✅ **并发控制设计合理**: 用 `HashSet` + scopeguard 防止同一 Key 重复查询
2. ✅ **事件日志完整**: 成功/失败都记录，满足「失败必须可见」原则
3. ✅ **错误分类清晰**: `Err` 只用于"调用方用错"（Key 不存在），查询失败用 `BalanceResult::failed`
4. ✅ **超时保护**: 默认 10 秒，用户可配置
5. ✅ **占位符展开**: 支持 `{{baseUrl}}`/`{{origin}}`/`{{apiKey}}`/`{{accessToken}}`/`{{userId}}`，覆盖各类站点

---

## 测试建议

建议补充以下单元测试（`src-tauri/src/lib.rs` 的 `#[cfg(test)] mod tests`）:

1. `query_key_balance` 对不存在 key_id 返回 `NotFound`
2. `query_key_balance` 对 `balance_query = None` 返回 `failed`
3. `get_balance_cache` 对 `queried_at = 0` 返回 `None`
4. `get_balance_cache` 对未来时间戳的行为（明确预期）
5. `update_balance_cache` 对已删除 Key 不 panic

（注：这些逻辑都在生产代码里正确处理，测试是为了锁定行为、防止未来误改）
