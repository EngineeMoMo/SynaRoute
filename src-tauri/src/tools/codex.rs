//! Codex 接入：把本地代理端点写进 `~/.codex/config.toml` 的自定义 provider。
//!
//! 从 `tools.rs` 抽出来的理由不只是体积（虽然那也是硬约束 —— tools.rs 的棘轮余量为 0）：
//! Codex 这条路上的判据密度远高于另两个客户端，而它们此前散在 tools.rs 的四个不相邻区段里，
//! 「占位符是不是我们写的」这一个语义就被抄成了三份（见 [`auth_carries_our_placeholder`]）。
//!
//! # 2026-08-25 实测判据矩阵（codex-cli 0.148.0-alpha.9，隔离 `CODEX_HOME` + 本地探针抓
//! `Authorization` 头）—— 本模块的每个决定都出自这张表，不是文档推测
//!
//! | config.toml | auth.json | 实际 base_url | 实际 Authorization |
//! |---|---|---|---|
//! | 我们的 provider + `experimental_bearer_token` | 有（真 OAuth） | provider 的 base_url | `Bearer <bearer>` ——**auth.json 被忽略** |
//! | bearer + `requires_openai_auth = true` | **无** | provider 的 base_url | `Bearer <bearer>`，**没有凭据门禁** |
//! | bearer + `requires_openai_auth` 省略 | **无** | provider 的 base_url | `Bearer <bearer>`（与上一行**逐字节相同**） |
//! | bearer + `requires_openai_auth = false` | **无** | provider 的 base_url | `Bearer <bearer>`（同上） |
//! | provider 表无 bearer，`requires_openai_auth = false` | 有 | provider 的 base_url | `Bearer <auth.json 的 key>`（本版**仍然继承**） |
//! | provider 表无 bearer，`requires_openai_auth = true` | 有 | provider 的 base_url | `Bearer <auth.json 的 key>` |
//! | **顶层 `model_provider` 键整个缺失** | 有占位符 | `https://api.openai.com/v1` | `Bearer synaroute-proxy` → **401** |
//! | `model_provider="synaroute"` 但**表缺失** | 任意 | —— | `Error: Model provider \`synaroute\` not found`，一个请求都不发 |
//! | `[model_providers.openai]` 想覆盖内置 id | —— | —— | 启动即失败：`reserved built-in provider IDs` |
//!
//! 由此三条结论，逐条对应本模块的一个设计：
//!
//! 1. **`experimental_bearer_token` 优先于 auth.json，且没有凭据门禁** → 我们**不写 auth.json**。
//!    那份占位符在正常接入时从不外发，纯粹是负债：它只在漂移之后才会被发出去，而收件人是
//!    **真实的 OpenAI**。用户看到 `Incorrect API key provided: synarout***roxy` 被指向
//!    platform.openai.com 查自己的 key —— 方向完全相反。不写它，这个失败模式从根上消失，
//!    顺带也不再动用户的 ChatGPT 登录态（旧实现是**整份替换** auth.json）。
//! 2. **真正把假 key 送去官方的判据是「顶层 `model_provider` 键缺失」**，不是历史注释写的
//!    「provider 表被丢掉而选中项留着」—— 后者 Codex 直接硬报错、不发请求。告警文案按形态分支
//!    （见 [`DriftState`]），否则会把人指去查一个根本不会发生的 401。
//! 3. **内置 provider id 不可覆盖** → 「把内置 `openai` 指向本地代理来中和回落」这条路已被证伪，
//!    别再试（Codex 启动即失败）。
//!
//! 另注意上表第 2~4 行：`requires_openai_auth` 的三种写法在本版**结果完全一样**。
//! 我们仍然写 `true`，理由是零代价的版本保险，判据在 [`apply_at`] 的文档里。
//!
//! # 版本前提（重要）
//!
//! 上表测的是**用户今天在跑的那个版本**（0.148.0-alpha.9）。这条路上已经有**两次**
//! 版本相关的行为漂移，都不是「当时写错了」：
//!
//! - 2026-08-02 的旧判据「`requires_openai_auth=true` 必须配套 auth.json，否则停在登录页」
//!   —— 今天测：`true` + 无 auth.json 照跑，无凭据门禁。
//! - 2026-08-26 用户报的升级公告「新版不再允许自定义 provider 在
//!   `requires_openai_auth=false` 时自动继承 auth.json 鉴权，报 `API_KEY_REQUIRED`」
//!   —— 今天测：上表第 5 行，`false` **仍然继承**。说明那是比本机更新的版本。
//!
//! 结论不是「谁对谁错」，而是**这个字段的语义会变**。故本模块的取舍一律按
//! **代价不对称**定，不按「我测过所以就这样」：万一某版本 `true` 仍要凭据，它报的是
//! `no Codex credentials were found · Run codex login`（响亮、可自助、OAuth 完好）；
//! 而写占位符 / 漏掉那个字段的代价是把假凭据发给第三方 + 一句指错方向的报错。

use crate::error::{AppError, AppResult};
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{
    backup_and_write_bytes, prerestore_path_for, read_preview_text, with_rollback,
    MCP_CLIENT_NAME, PROXY_PLACEHOLDER,
};

/// 写进 Codex `auth.json` 的 `OPENAI_API_KEY` 占位值。
///
/// # 为什么它和 [`PROXY_PLACEHOLDER`] 不是同一个值
///
/// 这一个**有可能被回显给用户**，另一个不会。桌面端 App 的登录门只认「auth.json 里有非空
/// `OPENAI_API_KEY`」（实测：对 `codex app-server` 直发 `getAuthStatus`，只有这一种形态才返
/// `authMethod: "apikey"`；不存在 / 只写 `auth_mode` / 空值三种都返 null → 弹登录页）。
/// 所以这份占位符必须存在。而一旦 `model_provider` 被第三方改掉、Codex 回落官方地址，
/// 它就会被发给**真实的 OpenAI**，换回一句 `Incorrect API key provided: ...`。
///
/// OpenAI 只回显**前 8 位 + 后 4~5 位**、中间打码（实测三种长度都是这个规则）。
/// 旧值 `synaroute-proxy` 显示成 `synarout***roxy` —— 看着就像个真 key，
/// 于是用户被那句「You can find your API key at platform.openai.com」引去查自己的密钥。
///
/// 现值让**可见的那两截**自己说话：显示成 `SEE-SYNA***ROUTE`，
/// 一眼就知道该去看 SynaRoute，而不是去 OpenAI 查 key。这不是花招 ——
/// 那句报错是这条链路上唯一必然到达用户眼前的文本，前 8 位是我们能控制的全部带宽。
pub(super) const CODEX_AUTH_PLACEHOLDER: &str =
    "SEE-SYNAROUTE-APP-THIS-IS-NOT-A-REAL-KEY-SYNAROUTE";

/// 旧版本写过的 auth.json 占位值。**只用于识别与清理，绝不再写入。**
///
/// 删掉它的代价：老用户盘上那份 `synaroute-proxy` 再也认不出来 → 会被当成「用户自己的真 key」
/// 而受到保护，于是清理与漂移告警同时哑掉，那份假 key 一直留在盘上等着被发给 OpenAI。
/// **识别面必须比写入面宽** —— 故这里的字面量是刻意的，不能改成引用现值
/// （`placeholder_has_a_single_source_of_truth` 那条门为此专门放过这一行）。
pub(super) const LEGACY_CODEX_AUTH_PLACEHOLDERS: &[&str] = &["synaroute-proxy"];


/// `~/.codex/config.toml`。
pub(super) fn config_path() -> AppResult<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| AppError::ToolConfig("无法定位用户目录".into()))?;
    Ok(home.join(".codex").join("config.toml"))
}

/// `~/.codex/auth.json`（与 config.toml 同目录）。
///
/// **我们不再往这里写任何东西**（见模块头）。保留这个路径只为两件事：
/// ① 写入让桌面端跳过登录页的占位符（见 [`apply_auth_at`]）；② 还原时解除它；
/// ③ 漂移检测时判断「那份占位符是否正被发往错误的地址」。
pub(super) fn auth_path() -> AppResult<PathBuf> {
    let cfg = config_path()?;
    Ok(cfg
        .parent()
        .ok_or_else(|| AppError::ToolConfig("无法定位 .codex 目录".into()))?
        .join("auth.json"))
}

// ---------------------------------------------------------------------------
// 接入
// ---------------------------------------------------------------------------

/// 写入 Codex 接入配置：`config.toml` + `auth.json`（后者仅在必要时，见 [`apply_auth_at`]）。
pub(super) fn apply(endpoint: &str, default_model: Option<&str>) -> AppResult<String> {
    let path = config_path()?;
    let auth = auth_path()?;
    // 两条路径都进 with_rollback。副文件（`.bak` / `.synaroute-created`）由 `with_rollback`
    // 自己按主路径推导后一并纳入快照 —— 漏了它们会造成数据丢失级后果，
    // 判据见 tools.rs 里 `with_rollback` 的文档。
    with_rollback(&[path.clone(), auth.clone()], || {
        let msg = apply_at(&path, endpoint, default_model)?;

        // **写完立刻读回校验**，且校验的是「身份」不是「存在」（见 `is_intact`）。
        //
        // 为什么不可省：Codex 桌面端 App 拥有 config.toml，会在接入**之后**重写它。
        // 这道门只保证「写入那一刻是对的」，之后靠 `drift_state` 常驻检测。
        if !is_intact(&path, endpoint) {
            return Err(AppError::ToolConfig(format!(
                "写入 Codex 配置后校验未通过：{} 里 model_provider 未选中 `{}`，\
                 或 [model_providers.{}] 的 base_url 不是本机代理地址（{}）。已回滚本次改动。\n\
                 常见原因：Codex 正在运行并重写了该文件 —— 请先完全退出 Codex 再重试接入。",
                path.display(),
                MCP_CLIENT_NAME,
                MCP_CLIENT_NAME,
                expected_base_url(endpoint)
            )));
        }

        let auth_msg = apply_auth_at(&auth)?;
        Ok(match auth_msg {
            Some(note) => format!("{msg}；{note}"),
            None => msg,
        })
    })
}

/// 让桌面端 App 跳过登录页所需的最小写入。返回 `Some(说明)` 表示确实写了。
///
/// # 为什么这份 auth.json 无法避免（2026-08-26 实测，此前删过一次、把用户推到了登录页）
///
/// 桌面端 App 每隔几秒发一次 `getAuthStatus` RPC，返 `authMethod: null` 就显示
/// 「登录 ChatGPT」页。直接对 `codex app-server` 发那个 RPC 逐形态实测：
///
/// | auth.json | `getAuthStatus` 返回 |
/// |---|---|
/// | **不存在** | `authMethod: null` → **弹登录页** |
/// | `{"auth_mode":"apikey"}`（只有模式、无 key） | `authMethod: null` → 仍弹 |
/// | `{"auth_mode":"apikey","OPENAI_API_KEY":""}`（空值） | `authMethod: null` → 仍弹 |
/// | `{"OPENAI_API_KEY":"<非空>"}` | `authMethod: "apikey"` → **不弹** |
///
/// 即：**唯一的钥匙是「非空 `OPENAI_API_KEY`」**，没有「只声明模式」这种不放值的写法。
/// 返回里那个 `requiresOpenaiAuth` 恒为 `true`，与我们写进 provider 表的
/// `requires_openai_auth` **无关**（试过 false / 省略，返回不变）—— 别指望用它绕过。
///
/// 注意这与「请求能不能发出去」是两件事：CLI 侧有 `experimental_bearer_token` 就够了，
/// 五种形态实测全部畅通、无凭据门禁。这份 auth.json **纯粹是为了那道 UI 门**。
/// 上一版把两件事混为一谈，删掉它之后 CLI 照跑、而桌面端用户卡在登录页。
///
/// # 为什么是「整份覆盖 + 备份」而不是「有真凭据就不碰」（2026-08-26 第二次改）
///
/// 上一版写的是「已有真凭据（OAuth `tokens` 或真 key）时一个字节都不动」，理由是
/// 「有真凭据时 App 本来就不弹登录页」。**那个前提是错的** —— 真机实测：用户盘上是一份
/// **过期的** ChatGPT OAuth（`Failed to refresh token: 你已登出或在其他账户登录`），
/// 而它的 `getAuthStatus` 返回 `authMethod: null` → 照样弹登录页。
///
/// | auth.json | `getAuthStatus` |
/// |---|---|
/// | 过期 OAuth（`auth_mode:chatgpt` + 刷不动的 `tokens`） | `null` → **弹登录页** |
/// | 只有非空 `OPENAI_API_KEY`（cc-switch 切中转站时写的形态） | `"apikey"` → 不弹 |
/// | 非空 key + `auth_mode:"apikey"`（我们写的形态） | `"apikey"` → 不弹 |
/// | 不存在 | `null` → 弹 |
///
/// 也就是说「保着那份 OAuth」并没有保住任何**可用**的东西 —— 它对 Codex 已经等于没有，
/// 而代价是用户永远停在登录页。cc-switch 的做法（从它本机库取证）正是**整份覆盖**：
/// 它的中转站档写的是 `{"OPENAI_API_KEY":"sk-…"}`，**没有** `tokens`、没有 `auth_mode:chatgpt`。
///
/// 所以判据从「能不能写」改成「**写之前有没有留下可回滚的副本**」：
/// - `backup_and_write_bytes` 首写即锁地把**接入前的原件**存进 `.bak`；
/// - `restore` / 停止代理时整份交还（`disarm_legacy_placeholder_auth` 的第一支）。
///
/// 这与「不碰」的区别是**可逆 vs 不可用**：覆盖是可逆的（原件在 `.bak`，用户点停止就回来），
/// 而「不碰」换来的是一个不可用的登录页。两害相权，可逆的那个。
///
/// # 那条数据丢失缺陷仍然被防着（别把这次改动当成回退）
///
/// 上一版引入白名单是为了防「Codex 0.149 把 OAuth 挪进混淆键名 → 黑名单判否 → 覆盖后
/// 盘上无副本」。真正致命的不是「覆盖」，是**覆盖时没留副本**。故这次保留白名单，
/// 但把它的用途收窄到它本来该管的那件事：[`auth_is_only_our_placeholder`] 决定
/// 「这份文件能不能**整份删掉**」（不可逆操作，必须保守）。
/// 而「能不能覆盖」由「备份是否成功」决定 —— `backup_and_write_bytes` 失败即整个 apply 失败回滚。
///
/// # 三条防线（因为这份假凭据确实有外发风险）
///
/// 它只在「`model_provider` 被第三方改掉、Codex 回落官方地址」时才会被发出去。故：
/// 1. **占位符自解释**：那句 401 显示成 `SEE-SYNA***OUTE`（见 `CODEX_AUTH_PLACEHOLDER`），
///    把人指向 SynaRoute 而不是 platform.openai.com；
/// 2. **漂移检测**：`drift_state` 常驻盯着（启动即跑 + 60s 一轮 + 系统通知）；
/// 3. **还原时解除**：`restore` 交还原件并删 `.bak`。
fn apply_auth_at(path: &Path) -> AppResult<Option<String>> {
    // 已经是我们的占位符 → 幂等短路。**必须有这一支**：不短路的话
    // `backup_and_write_bytes` 会把「已接入态」当成「接入前快照」拷进 `.bak`，
    // 此后点还原拿回来的就是那个假 key（旧实现的 `obj.len() == 1` 判据被两字段形态击穿，
    // 正是这么漏的）。
    if auth_carries_our_placeholder(path) {
        return Ok(None);
    }

    // 用户自己的**可用**真 key（非占位符、非 OAuth）→ 不碰。
    //
    // 这一支与 OAuth 那种情形不同：一个真实的 api key 对 Codex 是**有效**的，
    // `getAuthStatus` 会返 `"apikey"`、本来就不弹登录页，覆盖它纯属破坏。
    // 而过期 OAuth 是「看着在、实际等于没有」，那种才需要覆盖。
    if auth_has_usable_api_key(path) {
        return Ok(None);
    }

    let had_oauth = auth_has_oauth_tokens(path);
    let body = serde_json::json!({
        "OPENAI_API_KEY": CODEX_AUTH_PLACEHOLDER,
        // 与 `codex login --with-api-key` 写出的形态一致（它会补这个字段）。
        // 写上它不影响那道门（门只看 key 非空），但让盘上的文件看起来是正常形态、
        // 而不是一个来源不明的孤立字段。
        "auth_mode": "apikey",
    });
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| AppError::ToolConfig(format!("序列化 auth.json 失败: {e}")))?;
    // 备份失败即上抛 → `apply` 的 with_rollback 把整轮改动回滚。
    // 「没留下副本就绝不覆盖」这条是本函数唯一不可让的判据。
    backup_and_write_bytes(path, &bytes)?;
    if had_oauth {
        return Ok(Some(format!(
            "已写入 {}（占位密钥，仅为让 Codex 桌面端跳过登录页）；\
             原 ChatGPT 登录态已备份，点「停止」或「还原」即可交还",
            path.display()
        )));
    }
    Ok(Some(format!(
        "已写入 {}（占位密钥，仅为让 Codex 桌面端跳过登录页；真实密钥由代理按路由 Key 注入）",
        path.display()
    )))
}

/// auth.json 里是否有一个**可用的、属于用户自己的** API key。
///
/// 「可用」是关键限定：这个判据决定「要不要放弃覆盖」，而放弃覆盖的代价是用户停在登录页。
/// 故只在覆盖**确实是破坏**时才返回 true —— 也就是那份凭据对 Codex 真的有效。
///
/// # 为什么 OAuth `tokens` 不算在这里
///
/// 一份 OAuth 可能已经**刷不动了**（用户在别处登出/换号），此时 `getAuthStatus` 返
/// `authMethod: null`、照样弹登录页 —— 保着它没保住任何可用的东西。
/// 而 api key 不会「过期到无法判断」：它要么被上游接受、要么被拒，Codex 侧一律返 `"apikey"`、
/// 不弹登录页。两者性质不同，故分开判。
///
/// OAuth 的那份由 `.bak` 保护（覆盖前整份存档，还原时交还），见 [`apply_auth_at`]。
fn auth_has_usable_api_key(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v))
        if !v.is_empty() && !is_our_placeholder_value(v))
}

/// auth.json 里是否有 ChatGPT OAuth 令牌（不论是否还能刷新）。
///
/// 只用于**给用户的文案**（「原登录态已备份」），不参与「能不能写」的决策 ——
/// 那个判据在 [`auth_has_usable_api_key`]。
///
/// `!t.is_null()` 不能写成 `.is_some()`：登出后的形态是 `"tokens": null`，
/// 而 `.is_some()` 对 `Value::Null` 为真 —— 那会让文案谎称「已备份登录态」而其实没有。
fn auth_has_oauth_tokens(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    obj.get("tokens").is_some_and(|t| !t.is_null())
}

/// 某个 `OPENAI_API_KEY` 取值是否是我们写的占位符（含**历史**值）。
///
/// 识别面必须比写入面宽：老用户盘上是 `synaroute-proxy`，只认新值会让清理与告警
/// 同时哑掉，而那份假 key 会一直留着等被发给 OpenAI。
fn is_our_placeholder_value(v: &str) -> bool {
    v == CODEX_AUTH_PLACEHOLDER
        || LEGACY_CODEX_AUTH_PLACEHOLDERS.contains(&v)
}

/// provider 表里该写的 `base_url`：Codex 按 `wire_api=responses` 调 `{base_url}/responses`，
/// 而本地代理识别 `/v1/responses`，故要带上 `/v1`。
fn expected_base_url(endpoint: &str) -> String {
    format!("{}/v1", endpoint.trim_end_matches('/'))
}

/// 可测入口：写入指定 config.toml。
///
/// 写的五个字段各有判据：
/// - `model_provider = "synaroute"` 选中我们；
/// - `[model_providers.synaroute].base_url = {endpoint}/v1`；
/// - `wire_api = "responses"`（Codex 的 Responses 形态）；
/// - `experimental_bearer_token` = 占位符 —— 代理侧剥掉入站鉴权头、按路由 Key 注入真实密钥，
///   故此值只需非空。它是 `ModelProviderInfo` 的正式字段（codex.exe 符号表可见）。
/// - `requires_openai_auth = true` —— 见下。
///
/// # `requires_openai_auth = true` 是**保险**，不是需求（2026-08-26 用户报障后改回）
///
/// 本轮曾一度把它删掉，理由是「它把 Codex 推向 auth.json 那条凭据链，而我们要摆脱那条」。
/// **那个理由是错的**，实测矩阵（0.148.0-alpha.9，本地探针抓 `Authorization` 头，
/// 三种写法 × 有无 auth.json 共 5 组）显示：只要 `experimental_bearer_token` 在，
/// `true` / `false` / 省略**三者结果逐字节相同** —— 一律发 `Bearer <占位符>`、
/// 无凭据门禁、也不去读 auth.json。也就是说写它**不花任何代价**。
///
/// 而不写它有代价：用户报了一条**版本升级公告**级的现场信息 —— 新版 Codex 不再允许自定义
/// provider 在 `requires_openai_auth = false` 时自动继承 auth.json 的鉴权，
/// 症状是 `API_KEY_REQUIRED` / `401 Unauthorized`，官方给的解法就是把它改成 `true`。
/// 那条在本机装的 0.148 上**复现不出来**（实测 `false` + 无 bearer + 有 auth.json 仍然继承成功），
/// 说明它属于比本机更新的版本 —— 但正因为复现不出，才更不能赌。
///
/// 三条旁证都指向 `true`：cc-switch 生成的生效配置是 `true`；用户自己那份能正常工作的
/// `[model_providers.custom]` 也是 `true`；官方升级公告要求 `true`。
///
/// **代价不对称**（这才是决策依据，不是赌版本）：
/// - 万一某版本 `true` + 无 auth.json 会卡凭据门禁 → 报
///   `no Codex credentials were found · Run codex login`：响亮、可自助、OAuth 完好；
/// - 万一某版本不写它就不给鉴权 → 报 `API_KEY_REQUIRED` / 401：正是用户这一整轮在追的那个
///   看不懂的错误。
///
/// 有 `requires_openai_auth_is_written_as_true` 一条测试钉住它，别再顺手删。
///
/// **刻意仍不写 `env_key`**：那会让 Codex 改从环境变量读 key，重新引入「用户手设环境变量」负担。
///
/// 幂等：序列化结果与磁盘一致时 `backup_and_write_bytes` 短路（不备份不写），
/// 故重复接入不会把已接入的 config 当成「接入前快照」拷进 `.bak`。
/// 保留 config.toml 其余表（`mcp_servers` / 用户自己的 `model_providers` 等）不动。
pub(super) fn apply_at(
    path: &Path,
    endpoint: &str,
    default_model: Option<&str>,
) -> AppResult<String> {
    let content = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };

    let mut doc: toml::Value = if content.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        content
            .parse::<toml::Value>()
            .map_err(|e| AppError::ToolConfig(format!("解析 config.toml 失败: {e}")))?
    };

    let base_url = expected_base_url(endpoint);
    let table = doc
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("config.toml 顶层非表".into()))?;

    table.insert(
        "model_provider".to_string(),
        toml::Value::String(MCP_CLIENT_NAME.to_string()),
    );

    // 默认模型（借鉴 cc-switch）：写顶层 model，让 Codex 启动即有模型、无需 /model 手选。
    // 解析不出时**不动**用户已有的 model 字段（避免清空）。
    if let Some(m) = default_model.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        table.insert("model".to_string(), toml::Value::String(m.to_string()));
    }

    let providers = table
        .entry("model_providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let providers_table = providers
        .as_table_mut()
        .ok_or_else(|| AppError::ToolConfig("model_providers 非表".into()))?;
    let mut entry = toml::value::Table::new();
    entry.insert("name".into(), toml::Value::String("SynaRoute".into()));
    entry.insert("base_url".into(), toml::Value::String(base_url.clone()));
    entry.insert("wire_api".into(), toml::Value::String("responses".into()));
    entry.insert(
        "experimental_bearer_token".into(),
        toml::Value::String(PROXY_PLACEHOLDER.to_string()),
    );
    // `requires_openai_auth = true`：实测它对我们**完全无副作用**（bearer 在场时，
    // true/false/省略三者发出去的 Authorization 头逐字节相同、都不读 auth.json），
    // 而新版 Codex 对 `false` 的自定义 provider 会拒绝继承鉴权、报 `API_KEY_REQUIRED`。
    // 零代价的保险，写上。完整判据见本函数的文档注释。
    entry.insert("requires_openai_auth".into(), toml::Value::Boolean(true));
    providers_table.insert(MCP_CLIENT_NAME.to_string(), toml::Value::Table(entry));

    let serialized =
        toml::to_string_pretty(&doc).map_err(|e| AppError::ToolConfig(e.to_string()))?;
    backup_and_write_bytes(path, serialized.as_bytes())?;
    let model_note = default_model
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|m| format!("，默认模型={m}"))
        .unwrap_or_default();
    Ok(format!(
        "已写入 Codex 配置：{}（model_provider={MCP_CLIENT_NAME}，base_url={base_url}，\
         wire_api=responses{model_note}）；转发鉴权走 provider 表的 bearer 占位，\
         真实密钥由代理按路由 Key 注入；原文件已备份",
        path.display()
    ))
}

// ---------------------------------------------------------------------------
// 假凭据的识别与解除
// ---------------------------------------------------------------------------

/// auth.json 里的 `OPENAI_API_KEY` **是不是我们写的占位符**。
///
/// 这是「假 key 是否武装着」的**唯一判据**，漂移检测与清理都走它。
///
/// # 为什么不能是「整个对象恰好只有这一个键」
///
/// 旧实现是 `obj.len() == 1 && OPENAI_API_KEY == 占位符`。那个 `len() == 1` 是从另一个问题
/// （「这份文件能不能整份删掉」）借来的代理判据，两个问题不是一回事：
/// - 「能不能删」要保守（**绝不能**删掉含 OAuth `tokens` 的真凭据）→ 见 [`auth_is_only_our_placeholder`]；
/// - 「假 key 在不在」要宽松（多一个 `auth_mode` 字段不影响那个 key 会被发出去）。
///
/// 把保守判据用在宽松问题上，失效方向是**静默放行**（fail-open）：`codex login --with-api-key`
/// 写出的是 `{"auth_mode":"apikey","OPENAI_API_KEY":"…"}` **两个字段**，命中不了 `len()==1`，
/// 于是漂移告警与清理同时哑掉，而那个 key 照样在发。
pub(super) fn auth_carries_our_placeholder(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v)) if is_our_placeholder_value(v))
}

/// 这份 auth.json 是否**纯粹**是我们凭空造出来的占位符文件（可以整份删掉）。
///
/// 比 [`auth_carries_our_placeholder`] 严格：额外要求没有 OAuth `tokens`，也没有
/// 除 `OPENAI_API_KEY` / `auth_mode` 之外的任何字段。用于「删整个文件」这种不可逆操作。
///
/// 允许 `auth_mode` 共存的理由：`codex login --with-api-key` 会补上它，而那种文件里
/// **只有**我们的占位符，删掉它不会丢任何用户凭据。
pub(super) fn auth_is_only_our_placeholder(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if !matches!(obj.get("OPENAI_API_KEY"), Some(Value::String(v)) if is_our_placeholder_value(v)) {
        return false;
    }
    if obj.get("tokens").is_some_and(|t| !t.is_null()) {
        return false;
    }
    obj.keys()
        .all(|k| k == "OPENAI_API_KEY" || k == "auth_mode")
}

/// 解除 auth.json 里的占位符（本轮写的、或旧版本留下的）。返回 `Some(说明)` 表示确实动了。
///
/// 三种情形，都要留下**可回滚的现场**（`.synaroute-prerestore`），与 `restore_one` 同一纪律：
/// - `.bak` 在（旧版接入时备份过真凭据）→ 交还真凭据，删 `.bak`；
/// - 无 `.bak` 且文件纯粹是占位符 → 删掉整个文件（回到「接入前没有 auth.json」）；
/// - 无 `.bak` 但文件里还有别的东西（用户后来自己 `codex login` 过）→ 只摘掉那个 key，
///   其余字段原样保留。**绝不整份删**。
pub(super) fn disarm_legacy_placeholder_auth(path: &Path) -> AppResult<Option<String>> {
    if !auth_carries_our_placeholder(path) {
        return Ok(None);
    }
    // 无论走哪支，先留现场：这一步失败不算致命（继续解除更重要），但要告警。
    if let Err(e) = std::fs::copy(path, prerestore_path_for(path)) {
        tracing::warn!("解除 Codex 占位凭据前保留现场失败 {}: {e}", path.display());
    }

    let backup = super::backup_path_for(path);
    if backup.exists() {
        let data = std::fs::read(&backup)?;
        crate::secret::atomic_write(path, &data)?;
        if let Err(e) = std::fs::remove_file(&backup) {
            tracing::warn!("解除后清理备份 {} 失败: {e}", backup.display());
        }
        return Ok(Some(format!(
            "已从备份交还 {}（占位密钥已解除，官方登录态恢复）",
            path.display()
        )));
    }

    if auth_is_only_our_placeholder(path) {
        std::fs::remove_file(path)?;
        // 凭空新建标记若还在，一并清掉：文件已不存在，标记留着只会让日后的 restore
        // 去删一份「用户后来自己登录出来的」真凭据（无 .bak 无 prerestore，静默丢失）。
        let marker = super::created_marker_path_for(path);
        if marker.exists() {
            if let Err(e) = std::fs::remove_file(&marker) {
                tracing::warn!("清理新建标记 {} 失败: {e}", marker.display());
            }
        }
        return Ok(Some(format!(
            "已删除 {}（接入时凭空创建的纯占位密钥文件）",
            path.display()
        )));
    }

    // 混合形态：只摘 key，保留用户自己的其余字段。
    let text = std::fs::read_to_string(path)?;
    let mut obj: serde_json::Map<String, Value> = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(o)) => o,
        _ => return Ok(None),
    };
    obj.remove("OPENAI_API_KEY");
    // `auth_mode: "apikey"` 失去了它指向的 key，留着会让 Codex 判定为 api-key 模式却找不到 key。
    if matches!(obj.get("auth_mode"), Some(Value::String(v)) if v == "apikey") {
        obj.remove("auth_mode");
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(obj))
        .map_err(|e| AppError::ToolConfig(format!("序列化 auth.json 失败: {e}")))?;
    crate::secret::atomic_write(path, &bytes)?;
    Ok(Some(format!(
        "已从 {} 摘除占位密钥（其余字段保留）",
        path.display()
    )))
}

// ---------------------------------------------------------------------------
// 完好性与漂移
// ---------------------------------------------------------------------------

/// 我们那套接入配置是否**完好** —— 判的是「身份」，不是「存在」。
///
/// 四条同时成立才算完好：
/// 1. 顶层 `model_provider` 选中 `synaroute`；
/// 2. `[model_providers.synaroute]` 表在；
/// 3. 它的 `base_url` **正是本机当前代理地址**；
/// 4. 它带着我们的 `experimental_bearer_token`。
///
/// # 为什么 3 和 4 不能省
///
/// 旧实现只查「`base_url` 非空」。于是
/// `[model_providers.synaroute] base_url = "https://relay.example/v1"`（或指着一个**已死的
/// 旧端口**）被判为完好 → 漂移告警永远不发。这不是假想形态：cc-switch 会把 SynaRoute
/// 的接入态整份存成它自己的一个 provider 档，用户在它那边切回来时写出的就是一个
/// **端口可能已经变了、且没有 bearer** 的 `[model_providers.synaroute]` ——
/// 此时占位符成了唯一凭据，而 `is_intact` 说一切正常。
///
/// 端口比对失配时的正确处置是**重新接入**（写上当前端口），不是报错；调用方据此决定。
pub(super) fn is_intact(path: &Path, endpoint: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml::Value>() else {
        return false;
    };
    let selected = doc
        .get("model_provider")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == MCP_CLIENT_NAME);
    if !selected {
        return false;
    }
    let Some(t) = doc
        .get("model_providers")
        .and_then(|v| v.get(MCP_CLIENT_NAME))
        .and_then(|v| v.as_table())
    else {
        return false;
    };
    let url_ok = t
        .get("base_url")
        .and_then(|b| b.as_str())
        .is_some_and(|s| s == expected_base_url(endpoint));
    let bearer_ok = t
        .get("experimental_bearer_token")
        .and_then(|b| b.as_str())
        .is_some_and(|s| s == PROXY_PLACEHOLDER);
    // `requires_openai_auth` 被改成 false（或删掉）也算不完好：新版 Codex 对 `false` 的
    // 自定义 provider 不再继承鉴权，报 `API_KEY_REQUIRED` / 401。把它纳入完好性判据，
    // 这类改动才会被漂移检测发现、并被下一次接入自动纠正回来；
    // 否则用户只能自己去 config.toml 里手改那一行（那正是他现在被迫做的事）。
    let auth_flag_ok = t
        .get("requires_openai_auth")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    url_ok && bearer_ok && auth_flag_ok
}

/// 漂移形态。文案按形态分支，因为**每种形态下 Codex 的实际行为不同**，
/// 而指错方向的告警比没有告警更糟（它会让用户去查一个根本不存在的问题）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DriftState {
    /// 接入完好。
    Intact,
    /// 我们从未接入过（或已还原）—— 此时 config 不指向我们完全正常，不该报警。
    NotApplied,
    /// 顶层 `model_provider` 键缺失或 `= "openai"` → Codex 回落内置官方地址。
    /// **只有这一支会出现那句 401 `Incorrect API key provided`**（且仅当假 key 还武装着）。
    FellBackToOfficial { placeholder_armed: bool },
    /// 选中了别人的 provider。`destination` 是那个 provider 的 base_url（可能拿不到）。
    /// `placeholder_would_be_sent` = 该表没有自带凭据 → 我们的占位符会被发往那个地址。
    SelectedElsewhere {
        provider_id: String,
        destination: Option<String>,
        placeholder_would_be_sent: bool,
    },
    /// 选中项悬空（表缺失）→ Codex **启动即硬报错**、一个请求都不发。文案不得提 401。
    SelectionDangling,
    /// 表在、选中项也是我们，但 `base_url` 指向别处或缺 bearer（端口漂移 / 被外部改写）。
    OurTablePointsElsewhere { destination: Option<String> },
    /// `$CODEX_HOME/*.config.toml` 存在 → `codex --profile <name>` 会**整条旁路**我们写的一切。
    ProfileShadowed { profiles: Vec<String> },
    /// 顶层遗留 `profile = "..."` 老写法 → 当前 Codex **整份配置加载失败**。
    LegacyProfileKeyBreaksLoad { profile: String },
}

/// 判定漂移形态。`applied` = 我们是否认为自己处于已接入态（由调用方按 `.bak`/运行态给出）。
///
/// 判据刻意**不是**「占位符在不在 auth.json 里」（旧实现如此）：本版已经不写 auth.json 了，
/// 那个门一旦成为唯一入口，整个漂移检测就会变成永不触发的死代码 —— 而检测器自己静默失效，
/// 正是本仓反复吃过的那类亏。
pub(super) fn drift_state(
    cfg_path: &Path,
    auth_path: &Path,
    endpoint: &str,
    applied: bool,
) -> DriftState {
    // 配置加载层面的问题优先报：它们让 config.toml 里写了什么都无关紧要。
    if let Some(p) = legacy_profile_key(cfg_path) {
        return DriftState::LegacyProfileKeyBreaksLoad { profile: p };
    }
    let profiles = shadowing_profiles(cfg_path);
    if !profiles.is_empty() {
        return DriftState::ProfileShadowed { profiles };
    }

    if is_intact(cfg_path, endpoint) {
        return DriftState::Intact;
    }
    if !applied {
        return DriftState::NotApplied;
    }

    let armed = auth_carries_our_placeholder(auth_path);
    let doc = std::fs::read_to_string(cfg_path)
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok());
    let selected = doc
        .as_ref()
        .and_then(|d| d.get("model_provider"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let table_of = |id: &str| -> Option<&toml::value::Table> {
        doc.as_ref()?
            .get("model_providers")?
            .get(id)?
            .as_table()
    };

    match selected.as_deref() {
        // 键缺失，或显式选了内置 openai → 回落官方
        None | Some("openai") => DriftState::FellBackToOfficial {
            placeholder_armed: armed,
        },
        Some(id) if id == MCP_CLIENT_NAME => match table_of(id) {
            None => DriftState::SelectionDangling,
            Some(t) => DriftState::OurTablePointsElsewhere {
                destination: t
                    .get("base_url")
                    .and_then(|b| b.as_str())
                    .map(str::to_string),
            },
        },
        Some(id) => match table_of(id) {
            None => DriftState::SelectionDangling,
            Some(t) => {
                let has_own_credential = t.get("experimental_bearer_token").is_some()
                    || t.get("env_key").is_some();
                DriftState::SelectedElsewhere {
                    provider_id: id.to_string(),
                    destination: t
                        .get("base_url")
                        .and_then(|b| b.as_str())
                        .map(str::to_string),
                    placeholder_would_be_sent: armed && !has_own_credential,
                }
            }
        },
    }
}

/// 顶层遗留的 `profile = "..."`。当前 Codex 报
/// `legacy profile = "x" config is no longer supported`，**整份配置加载失败** ——
/// 此时我们写得再对也没用，而 `is_intact` 只用 toml 解析、看不出这件事。
fn legacy_profile_key(cfg_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cfg_path).ok()?;
    let doc = text.parse::<toml::Value>().ok()?;
    doc.get("profile")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `$CODEX_HOME` 下的 `<name>.config.toml` 清单。`codex --profile <name>` 读它、
/// **完全忽略** config.toml 的 `model_provider` —— 一条我们写什么都不生效的静默旁路。
fn shadowing_profiles(cfg_path: &Path) -> Vec<String> {
    let Some(dir) = cfg_path.parent() else {
        return Vec::new();
    };
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".config.toml").map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

/// 漂移告警文案。`None` = 无需告警。
///
/// 每支都必须说清三件事：**Codex 现在实际在做什么**、**这不是用户的密钥有问题**、
/// **怎么自助修**。不同支的第一件事不同，这正是要分支的原因。
pub(super) fn drift_warning(state: &DriftState) -> Option<String> {
    const FIX: &str = "处理：完全退出 Codex，再在本页点「停止」后重新「启动」以重写接入配置。";
    Some(match state {
        DriftState::Intact | DriftState::NotApplied => return None,

        DriftState::FellBackToOfficial { placeholder_armed } => {
            let head = "Codex 接入已失效：config.toml 里的顶层 `model_provider` 已被其它程序\
                 （Codex 自身或 cc-switch）改掉或删掉，Codex 正回落到官方 \
                 https://api.openai.com/v1，你的请求**没有走 SynaRoute**。";
            if *placeholder_armed {
                format!(
                    "{head}\n而 auth.json 里还留着 SynaRoute 写入的占位密钥（它只为让桌面端跳过登录页），\
                     于是 Codex 会把它发给官方，报「Incorrect API key provided: synarout***roxy」\
                     —— **那不是你的 OpenAI 密钥有问题**。\n{FIX}\
                     （在本页点「停止」会自动摘除这份占位密钥；重新「启动」会重写正确的接入配置。）"
                )
            } else {
                format!("{head}\n{FIX}")
            }
        }

        DriftState::SelectedElsewhere {
            provider_id,
            destination,
            placeholder_would_be_sent,
        } => {
            let dest = destination.as_deref().unwrap_or("（该 provider 未写 base_url）");
            let head = format!(
                "Codex 当前选中的是 provider `{provider_id}`（{dest}），\
                 不是 SynaRoute —— 你的请求没有走本地代理，故障转移/模型映射/用量统计都不生效。"
            );
            if *placeholder_would_be_sent {
                format!(
                    "{head}\n且该 provider 没有自带密钥，Codex 会把 auth.json 里 SynaRoute \
                     写入的占位密钥发往 {dest}，那边会回「密钥无效」。\n{FIX}"
                )
            } else {
                format!("{head}\n若这是你自己在 cc-switch 等工具里切换的结果，可忽略。\n{FIX}")
            }
        }

        DriftState::SelectionDangling => format!(
            "Codex 配置自相矛盾：顶层 `model_provider` 选中的 provider 在 \
             `[model_providers.*]` 里不存在。此时 Codex **启动即报错** \
             `Error: Model provider ... not found`，一个请求都发不出去 —— \
             所以你看到的不会是鉴权报错，而是压根打不开。\n{FIX}"
        ),

        DriftState::OurTablePointsElsewhere { destination } => format!(
            "Codex 里 SynaRoute 的 provider 表被改过：base_url 现在是 {} —— \
             不是本机代理的当前地址。最常见的成因是**代理端口变了而客户端配置没跟上**\
             （或被 cc-switch 写回了一份旧快照）。请求会打向那个地址，通常表现为连不上。\n{FIX}",
            destination.as_deref().unwrap_or("（空）")
        ),

        DriftState::ProfileShadowed { profiles } => format!(
            "检测到 Codex profile 配置：{}。用 `codex --profile <名字>` 启动时，Codex 读的是 \
             `<名字>.config.toml`，会**完全忽略** SynaRoute 写入的 `model_provider` —— \
             这条路上接入不生效，且不会有任何报错。若你平时用 profile 启动 Codex，\
             请把 `[model_providers.{}]` 与 `model_provider = \"{}\"` 一并写进那份 profile 文件。",
            profiles.join("、"),
            MCP_CLIENT_NAME,
            MCP_CLIENT_NAME
        ),

        DriftState::LegacyProfileKeyBreaksLoad { profile } => format!(
            "Codex 配置无法加载：config.toml 顶层还留着老写法 `profile = \"{profile}\"`，\
             当前版本的 Codex 会直接报 \
             `legacy profile config is no longer supported; use --profile {profile} with \
             {profile}.config.toml instead` 并拒绝启动。此时 SynaRoute 写什么都不生效。\n\
             处理：删掉 config.toml 里那一行 `profile = \"{profile}\"`。"
        ),
    })
}

// ---------------------------------------------------------------------------
// 预览
// ---------------------------------------------------------------------------

/// Codex 的工具配置预览（设置面板 / 分类页常驻告警都读它）。
///
/// `applied`（我们是否认为自己处于已接入态）由 [`believed_applied`] 从磁盘推导，
/// 不从前端传 —— 前端传进来的是挂载时的旧快照，而这里判的恰恰是「磁盘现在是什么样」。
pub(super) fn preview(endpoint: &str) -> AppResult<super::ToolConfigPreview> {
    let cfg = config_path()?;
    let auth = auth_path()?;
    // config.toml 必须脱敏：它可能含用户自己配的其它 provider 的 api_key / env_key，
    // 以及 MCP server 的环境变量。此前整份明文回显 —— 同一段密钥放 settings.json 会打码、
    // 放 config.toml 却不打码，口径分叉且泄露。
    let (cfg_exists, cfg_content) = read_preview_text(&cfg, true)?;
    let (auth_exists, auth_content) = read_preview_text(&auth, true)?;
    let state = drift_state(&cfg, &auth, endpoint, believed_applied(&cfg));
    Ok(super::ToolConfigPreview {
        category_id: crate::model::CategoryType::Codex,
        summary: "Codex：只写 ~/.codex/config.toml（model_provider=synaroute、\
                  [model_providers.synaroute] 含 base_url/wire_api/bearer 占位、可选顶层 model）。\
                  **不写 auth.json**，官方 ChatGPT 登录态原样保留。不写任何 ANTHROPIC_*。"
            .into(),
        mcp_registered: super::is_mcp_registered(crate::model::CategoryType::Codex),
        takeover_warning: drift_warning(&state),
        files: vec![
            super::ToolConfigFilePreview {
                path: cfg.display().to_string(),
                exists: cfg_exists,
                format: "toml".into(),
                content: cfg_content,
            },
            super::ToolConfigFilePreview {
                path: auth.display().to_string(),
                exists: auth_exists,
                format: "json".into(),
                content: auth_content,
            },
        ],
    })
}

/// 我们是否**认为**自己处于已接入态：`config.toml` 的 `.synaroute.bak` 或
/// 「凭空新建」标记存在。
///
/// 这两个副文件的语义就是「SynaRoute 动过这个文件、还没还原」（`restore_one` 成功后会删掉
/// 它们）。用它当判据而不是「代理在不在跑」：漂移的危险窗口恰恰覆盖「应用没开着」的那段
/// —— 用户几天没开 SynaRoute，某天打开 Codex 就撞上 401。
fn believed_applied(cfg_path: &Path) -> bool {
    super::backup_path_for(cfg_path).exists() || super::created_marker_path_for(cfg_path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用临时文件（pid + 自增序号，避免并发用例互踩 —— 与 tools.rs 的 `temp_file` 同一手法。
    /// 刻意不用时间戳：本机实测 `timestamp_nanos` 的量化粒度只有 100ns，
    /// 并发下撞名率极高，见 CLAUDE.md 里 `db_copy_path` 那条）。
    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "synaroute_codex_test_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const EP: &str = "http://127.0.0.1:47101";

    fn write(p: &Path, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    // ---- 接入写出的内容 ----

    /// `apply_at` 只写 config.toml，**它自己不碰 auth.json**（那是 `apply_auth_at` 的事）。
    ///
    /// 两件事分开的理由：转发只需要 provider 表里的 bearer（CLI 五种形态实测全通），
    /// 而 auth.json 纯粹是为了桌面端那道 UI 登录门。上一版把两者混为一谈、整个删掉，
    /// 结果 CLI 照跑而桌面端用户卡在登录页。
    #[test]
    fn apply_at_writes_config_only_and_does_not_touch_auth_json() {
        let d = temp_dir("apply_cfg_only");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        let msg = apply_at(&cfg, EP, Some("gpt-5.6-sol")).unwrap();
        assert!(!auth.exists(), "apply_at 这一层不该创建 auth.json");

        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(doc["model_provider"].as_str(), Some("synaroute"));
        assert_eq!(doc["model"].as_str(), Some("gpt-5.6-sol"));
        let p = &doc["model_providers"]["synaroute"];
        assert_eq!(p["base_url"].as_str(), Some("http://127.0.0.1:47101/v1"));
        assert_eq!(p["wire_api"].as_str(), Some("responses"));
        assert_eq!(p["experimental_bearer_token"].as_str(), Some(PROXY_PLACEHOLDER));
        assert!(
            p.get("env_key").is_none(),
            "env_key 会让 Codex 改从环境变量读 key，重新引入「用户手设环境变量」负担"
        );

        // 成功文案不得自相矛盾。踩过的坑：旧实现把「未改动 auth.json」与
        // 「已写入 …auth.json（鉴权占位）」用 format! 拼进同一句给用户看 ——
        // 同一句话既说没动又说写了。故这一层**压根不许提** auth.json 的去向，
        // 那句话由 `apply_auth_at` 按实际做了什么来出。
        assert!(
            !msg.contains("auth.json"),
            "config 层的文案不该断言 auth.json 的去向（它不知道）：{msg}"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// `requires_openai_auth` 必须写成 `true`，**且升级老配置时也要补上**。
    ///
    /// 本轮曾一度删掉它（理由是「它把 Codex 推向 auth.json 那条凭据链」）——
    /// 那个理由被实测证伪：bearer 在场时 true/false/省略三者发出的 Authorization 头
    /// **逐字节相同**，都不读 auth.json。也就是说写它零代价。
    ///
    /// 而不写它有代价：新版 Codex 对 `requires_openai_auth = false` 的自定义 provider
    /// 不再继承鉴权，报 `API_KEY_REQUIRED` / `401 Unauthorized`，官方解法就是改成 `true`。
    /// 那正是用户这一整轮在追的那类看不懂的 401。
    ///
    /// 这条测试的存在意义就是**别再顺手删它**。
    #[test]
    fn requires_openai_auth_is_written_as_true() {
        let d = temp_dir("roa_true");
        let cfg = d.join("config.toml");

        // ① 全新写入
        apply_at(&cfg, EP, None).unwrap();
        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(
            doc["model_providers"]["synaroute"]["requires_openai_auth"].as_bool(),
            Some(true),
            "缺了它，新版 Codex 会报 API_KEY_REQUIRED / 401"
        );

        // ② 老配置里被写成 false（或被别的工具改成 false）→ 必须被纠正成 true。
        //    这一支是「只对新用户生效的修复」那个坑的防线：老用户不重装也要拿到修复。
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\n\
             name = \"SynaRoute\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\n\
             wire_api = \"responses\"\n\
             requires_openai_auth = false\n",
        );
        apply_at(&cfg, EP, None).unwrap();
        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(
            doc["model_providers"]["synaroute"]["requires_openai_auth"].as_bool(),
            Some(true),
            "false 必须被纠正成 true"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 接入保留 config.toml 里其它一切（用户自己的 provider、MCP 注册、features 段）。
    #[test]
    fn apply_preserves_unrelated_tables() {
        let d = temp_dir("preserve");
        let cfg = d.join("config.toml");
        write(
            &cfg,
            "disable_response_storage = true\n\
             [features]\njs_repl = false\n\
             [model_providers.custom]\nname = \"别人\"\nbase_url = \"https://x.example/v1\"\n\
             [mcp_servers.synaroute]\ncommand = \"x.exe\"\n",
        );
        apply_at(&cfg, EP, None).unwrap();
        let doc = std::fs::read_to_string(&cfg).unwrap().parse::<toml::Value>().unwrap();
        assert_eq!(doc["disable_response_storage"].as_bool(), Some(true));
        assert_eq!(doc["features"]["js_repl"].as_bool(), Some(false));
        assert_eq!(doc["model_providers"]["custom"]["base_url"].as_str(), Some("https://x.example/v1"));
        assert!(doc["mcp_servers"]["synaroute"].get("command").is_some());
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- is_intact 判的是身份，不是存在 ----

    /// 旧实现只查「base_url 非空」。指向第三方、或指着一个**已死的旧端口**都会被判完好，
    /// 于是漂移告警永远发不出来。cc-switch 存的 SynaRoute 档正是这个形状（无 bearer）。
    #[test]
    fn intact_requires_our_endpoint_and_our_bearer() {
        let d = temp_dir("intact");
        let cfg = d.join("config.toml");

        apply_at(&cfg, EP, None).unwrap();
        assert!(is_intact(&cfg, EP), "刚写完必须完好");

        // 端口漂移：非空但不是我们
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\nbase_url = \"http://127.0.0.1:9999/v1\"\n\
             experimental_bearer_token = \"synaroute-proxy\"\n",
        );
        assert!(!is_intact(&cfg, EP), "base_url 不是本机当前端点 → 不完好");

        // 指向第三方且没有 bearer（cc-switch 写回的形状）
        write(
            &cfg,
            "model_provider = \"synaroute\"\n\
             [model_providers.synaroute]\nbase_url = \"https://relay.example/v1\"\n",
        );
        assert!(!is_intact(&cfg, EP), "指向第三方 + 无 bearer → 绝不能判完好");

        // 缺 bearer（占位符会成为唯一凭据）
        write(
            &cfg,
            &format!(
                "model_provider = \"synaroute\"\n\
                 [model_providers.synaroute]\nbase_url = \"{}/v1\"\n",
                EP
            ),
        );
        assert!(!is_intact(&cfg, EP), "缺 bearer → 不完好");

        // 选中别人
        write(&cfg, "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"https://x/v1\"\n");
        assert!(!is_intact(&cfg, EP));

        // `requires_openai_auth` 被改成 false → 新版 Codex 报 API_KEY_REQUIRED / 401。
        // 必须判不完好，否则漂移检测看不见它、下一次接入也不会把它纠正回来，
        // 用户只能自己去 config.toml 里手改那一行。
        write(
            &cfg,
            &format!(
                "model_provider = \"synaroute\"\n\
                 [model_providers.synaroute]\nbase_url = \"{EP}/v1\"\n\
                 experimental_bearer_token = \"synaroute-proxy\"\n\
                 requires_openai_auth = false\n"
            ),
        );
        assert!(!is_intact(&cfg, EP), "requires_openai_auth = false → 不完好");
        // 整个键被删掉，同理
        write(
            &cfg,
            &format!(
                "model_provider = \"synaroute\"\n\
                 [model_providers.synaroute]\nbase_url = \"{EP}/v1\"\n\
                 experimental_bearer_token = \"synaroute-proxy\"\n"
            ),
        );
        assert!(!is_intact(&cfg, EP), "requires_openai_auth 缺失 → 不完好");

        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 占位符判据：宽松 vs 严格 ----

    /// `codex login --with-api-key` 写出的是**两个字段**。旧判据 `obj.len() == 1` 判否，
    /// 失效方向是**静默放行**：假 key 照样在发，而告警与清理同时哑掉。
    #[test]
    fn two_field_auth_from_codex_login_is_still_recognized_as_ours() {
        let d = temp_dir("twofield");
        let auth = d.join("auth.json");

        write(&auth, r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synaroute-proxy"}"#);
        assert!(auth_carries_our_placeholder(&auth), "两字段形态必须被认出来");
        assert!(auth_is_only_our_placeholder(&auth), "只有我们的 key + auth_mode → 可整份删");

        // 单字段（旧版 SynaRoute 自己写的）同样认得
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        assert!(auth_carries_our_placeholder(&auth));
        assert!(auth_is_only_our_placeholder(&auth));

        std::fs::remove_dir_all(&d).ok();
    }

    /// 真实凭据在任何形态下都不能被当成我们的（这是「删整个文件」那条路的护栏）。
    #[test]
    fn real_credentials_are_never_treated_as_ours() {
        let d = temp_dir("realcred");
        let auth = d.join("auth.json");

        // ChatGPT OAuth
        write(
            &auth,
            r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{"access_token":"x"}}"#,
        );
        assert!(!auth_carries_our_placeholder(&auth));
        assert!(!auth_is_only_our_placeholder(&auth));

        // 用户自己的真实 api key
        write(&auth, r#"{"OPENAI_API_KEY":"sk-realkey","auth_mode":"apikey"}"#);
        assert!(!auth_carries_our_placeholder(&auth));
        assert!(!auth_is_only_our_placeholder(&auth));

        // 我们的占位符 + OAuth tokens 并存（混合态）：认得出是我们的 key（要摘），
        // 但**不可**整份删（会连 tokens 一起丢）。
        write(
            &auth,
            r#"{"OPENAI_API_KEY":"synaroute-proxy","tokens":{"access_token":"x"}}"#,
        );
        assert!(auth_carries_our_placeholder(&auth), "key 是我们的 → 要摘");
        assert!(
            !auth_is_only_our_placeholder(&auth),
            "含 OAuth tokens → 绝不能整份删"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 解除旧占位符 ----

    /// 有 `.bak`（旧版接入时备份过真凭据）→ 交还真凭据并删 .bak。
    #[test]
    fn disarm_restores_real_credentials_from_backup() {
        let d = temp_dir("disarm_bak");
        let auth = d.join("auth.json");
        let bak = super::super::backup_path_for(&auth);
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        write(&bak, r#"{"auth_mode":"chatgpt","tokens":{"access_token":"real"}}"#);

        let note = disarm_legacy_placeholder_auth(&auth).unwrap();
        assert!(note.is_some(), "确实动了就要有说明");
        let after = std::fs::read_to_string(&auth).unwrap();
        assert!(after.contains("real"), "真凭据必须交还：{after}");
        assert!(!bak.exists(), "交还后删掉 .bak");
        assert!(
            prerestore_path_for(&auth).exists(),
            "改写前必须留现场（可回滚）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 无 `.bak` 且文件纯粹是占位符 → 删掉整个文件，并清掉「凭空新建」标记。
    ///
    /// 标记不清的后果是数据丢失级：日后 restore 会走 marker 支路 `remove_file`，
    /// 删掉用户**后来自己 `codex login` 出来的**真凭据，而那条支路不留 prerestore。
    #[test]
    fn disarm_deletes_pure_placeholder_and_clears_created_marker() {
        let d = temp_dir("disarm_pure");
        let auth = d.join("auth.json");
        let marker = super::super::created_marker_path_for(&auth);
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        write(&marker, "");

        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_some());
        assert!(!auth.exists(), "纯占位符文件应整份删除");
        assert!(!marker.exists(), "凭空新建标记必须一并清掉，否则日后会误删真凭据");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 混合形态（用户后来自己 login 过）→ 只摘我们的 key，其余字段保留。
    #[test]
    fn disarm_only_removes_our_key_from_a_mixed_auth_file() {
        let d = temp_dir("disarm_mixed");
        let auth = d.join("auth.json");
        write(
            &auth,
            r#"{"OPENAI_API_KEY":"synaroute-proxy","auth_mode":"apikey","tokens":{"access_token":"real"}}"#,
        );

        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_some());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
        assert!(v.get("OPENAI_API_KEY").is_none(), "我们的 key 必须摘掉");
        assert!(v["tokens"]["access_token"].as_str() == Some("real"), "用户的 tokens 必须保留");
        assert!(
            v.get("auth_mode").is_none(),
            "auth_mode=apikey 失去了它指向的 key，留着会让 Codex 判成 api-key 模式却找不到 key"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 幂等：没有我们的占位符时什么都不做（也不留现场、不误删）。
    #[test]
    fn disarm_is_a_noop_when_there_is_nothing_of_ours() {
        let d = temp_dir("disarm_noop");
        let auth = d.join("auth.json");
        write(&auth, r#"{"auth_mode":"chatgpt","tokens":{"access_token":"real"}}"#);
        assert!(disarm_legacy_placeholder_auth(&auth).unwrap().is_none());
        assert!(auth.exists());
        assert!(!prerestore_path_for(&auth).exists(), "没动就不该留现场");

        // 文件不存在也不能报错
        let missing = d.join("nope.json");
        assert!(disarm_legacy_placeholder_auth(&missing).unwrap().is_none());
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 漂移分支 ----

    /// 每种漂移形态的文案必须点名**那个形态下 Codex 实际的行为**。
    /// 指错方向的告警比没有告警更糟：它会让用户去查一个根本不会发生的问题。
    #[test]
    fn drift_message_matches_what_codex_actually_does_in_each_shape() {
        let d = temp_dir("drift");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        // ① 完好 → 不报警
        apply_at(&cfg, EP, None).unwrap();
        assert_eq!(drift_state(&cfg, &auth, EP, true), DriftState::Intact);
        assert!(drift_warning(&DriftState::Intact).is_none());

        // ② 未接入 + config 不指向我们 → 不报警（噪音）
        write(&cfg, "model = \"x\"\n");
        assert_eq!(drift_state(&cfg, &auth, EP, false), DriftState::NotApplied);

        // ③ model_provider 键缺失 + 占位符武装 → 回落官方，**这一支才有那句 401**
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::FellBackToOfficial { placeholder_armed: true });
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("api.openai.com"), "要点名假 key 会发去哪：{w}");
        assert!(w.contains("不是你的 OpenAI 密钥有问题"), "必须纠正错误方向：{w}");

        // ③b 同形态但占位符已解除 → 仍要报「没走 SynaRoute」，但不得再提 401
        std::fs::remove_file(&auth).unwrap();
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::FellBackToOfficial { placeholder_armed: false });
        let w = drift_warning(&s).unwrap();
        assert!(!w.contains("Incorrect API key"), "占位符已解除就不该提那句 401：{w}");

        // ④ 选中第三方、该表自带 bearer → 占位符不外发，文案不得说「假密钥正被发往…」
        write(
            &cfg,
            "model_provider = \"custom\"\n[model_providers.custom]\n\
             base_url = \"https://relay.example/v1\"\nexperimental_bearer_token = \"sk-theirs\"\n",
        );
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(
            s,
            DriftState::SelectedElsewhere {
                provider_id: "custom".into(),
                destination: Some("https://relay.example/v1".into()),
                placeholder_would_be_sent: false,
            }
        );
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("relay.example"), "要点名真实去处：{w}");
        assert!(!w.contains("api.openai.com"), "不得指向 openai（会指错方向）：{w}");

        // ④b 该表没有自带凭据 → 占位符真的会被发过去
        write(
            &cfg,
            "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"https://relay.example/v1\"\n",
        );
        let s = drift_state(&cfg, &auth, EP, true);
        assert!(matches!(
            s,
            DriftState::SelectedElsewhere { placeholder_would_be_sent: true, .. }
        ));
        assert!(drift_warning(&s).unwrap().contains("占位密钥发往"));

        // ⑤ 选中项悬空 → Codex 硬报错、不发请求，文案**不得**提 401
        write(&cfg, "model_provider = \"synaroute\"\n");
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::SelectionDangling);
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("not found"), "要引用 Codex 的原话：{w}");
        assert!(!w.contains("401"), "这一支压根不会出现 401：{w}");

        // ⑥ 我们的表指向别处（端口漂移）
        write(
            &cfg,
            "model_provider = \"synaroute\"\n[model_providers.synaroute]\n\
             base_url = \"http://127.0.0.1:9999/v1\"\nexperimental_bearer_token = \"synaroute-proxy\"\n",
        );
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(
            s,
            DriftState::OurTablePointsElsewhere {
                destination: Some("http://127.0.0.1:9999/v1".into())
            }
        );
        assert!(drift_warning(&s).unwrap().contains("9999"));

        std::fs::remove_dir_all(&d).ok();
    }

    /// profile 旁路：两种都让「我们写的字段在位」变得毫无意义，而 toml 解析看不出来。
    #[test]
    fn profile_shapes_bypass_everything_we_write() {
        let d = temp_dir("profile");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");

        // `<name>.config.toml` 存在 → `codex --profile name` 完全忽略 config.toml
        apply_at(&cfg, EP, None).unwrap();
        write(&d.join("mine.config.toml"), "model = \"x\"\n");
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::ProfileShadowed { profiles: vec!["mine".into()] });
        let w = drift_warning(&s).unwrap();
        assert!(w.contains("mine"), "要点名那份 profile：{w}");
        assert!(w.contains("--profile"));
        std::fs::remove_file(d.join("mine.config.toml")).unwrap();

        // 顶层遗留 `profile = "..."` → 当前 Codex 整份配置加载失败
        let mut t = std::fs::read_to_string(&cfg).unwrap();
        t.insert_str(0, "profile = \"old\"\n");
        write(&cfg, &t);
        let s = drift_state(&cfg, &auth, EP, true);
        assert_eq!(s, DriftState::LegacyProfileKeyBreaksLoad { profile: "old".into() });
        assert!(drift_warning(&s).unwrap().contains("no longer supported"));

        std::fs::remove_dir_all(&d).ok();
    }

    /// 完好态下即使有 profile 文件也要报 —— 那条路上接入是**静默**不生效的，
    /// 而「静默」正是本仓最忌的形态。
    #[test]
    fn profile_shadowing_is_reported_even_when_our_config_is_intact() {
        let d = temp_dir("profile_intact");
        let cfg = d.join("config.toml");
        let auth = d.join("auth.json");
        apply_at(&cfg, EP, None).unwrap();
        assert_eq!(drift_state(&cfg, &auth, EP, true), DriftState::Intact);
        write(&d.join("work.config.toml"), "model = \"x\"\n");
        assert!(
            matches!(drift_state(&cfg, &auth, EP, true), DriftState::ProfileShadowed { .. }),
            "接入完好也要报 profile 旁路"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 用户自己**可用的** api key 不许动；而 OAuth 令牌**要覆盖，但必须先备份**。
    ///
    /// 这条替代了上一版的 `apply_auth_never_touches_real_credentials` —— 那条断言
    /// 「有 OAuth 就一个字节都不动」，而它的前提被真机证伪：用户盘上那份 OAuth 已经
    /// **刷不动了**（`Failed to refresh token: 你已登出或在其他账户登录`），
    /// `getAuthStatus` 返 `authMethod: null` → 照样弹登录页。
    /// 保着它没保住任何**可用**的东西，代价是用户永远停在登录页。
    ///
    /// 判据因此从「能不能写」改成「**写之前有没有留下可回滚的副本**」：
    /// 覆盖是可逆的（原件在 `.bak`，点停止就交还），而「不碰」换来的是一个不可用的登录页。
    #[test]
    fn apply_auth_overwrites_oauth_but_always_leaves_a_backup() {
        let d = temp_dir("oauth_backup");
        let auth = d.join("auth.json");
        let oauth = r#"{"OPENAI_API_KEY":null,"auth_mode":"chatgpt","tokens":{"access_token":"real","refresh_token":"rt"},"last_refresh":"2026-06-25T13:01:56Z"}"#;
        write(&auth, oauth);

        let note = apply_auth_at(&auth).unwrap().expect("过期 OAuth 必须被覆盖，否则一直弹登录页");
        assert!(
            note.contains("已备份"),
            "文案必须告诉用户原登录态还在、怎么拿回来：{note}"
        );

        // 覆盖后是能过那道门的形态（非空 key）
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
        assert_eq!(v["OPENAI_API_KEY"].as_str(), Some(CODEX_AUTH_PLACEHOLDER));
        assert!(v.get("tokens").is_none(), "覆盖后不该再留着 tokens 字段");

        // **最要紧的一条**：原件逐字节在 .bak 里，还原能拿回来。
        let bak = super::super::backup_path_for(&auth);
        assert!(bak.exists(), "覆盖前必须留下副本 —— 没副本就绝不许覆盖");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            oauth,
            ".bak 必须是**接入前**的原件，逐字节相同"
        );

        // 还原：交还原件、删 .bak
        disarm_legacy_placeholder_auth(&auth).unwrap();
        assert_eq!(
            std::fs::read_to_string(&auth).unwrap(),
            oauth,
            "还原必须把 ChatGPT 登录态逐字节交还"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 用户自己**可用的** api key 一个字节都不许动。
    ///
    /// 与 OAuth 那种情形性质不同：一个真实 api key 对 Codex 是**有效**的，
    /// `getAuthStatus` 返 `"apikey"`、本来就不弹登录页，覆盖它纯属破坏。
    #[test]
    fn apply_auth_never_touches_a_usable_api_key() {
        let d = temp_dir("keep_realkey");
        let auth = d.join("auth.json");
        let realkey = r#"{"OPENAI_API_KEY":"sk-userrealkey123","auth_mode":"apikey"}"#;
        write(&auth, realkey);

        assert_eq!(apply_auth_at(&auth).unwrap(), None, "可用的真 key → 什么都不做");
        assert_eq!(std::fs::read_to_string(&auth).unwrap(), realkey);
        assert!(
            !super::super::backup_path_for(&auth).exists(),
            "既然不动它，就不该为它建 .bak（建了反而是把真凭据复制了一份）"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 无凭据时才写占位符 —— 这是桌面端跳过登录页的唯一钥匙。
    ///
    /// 实测判据（对 `codex app-server` 直发 `getAuthStatus`）：只有非空 `OPENAI_API_KEY`
    /// 才返 `authMethod: "apikey"`；不存在 / 只写 `auth_mode` / 空值三种都返 null → 弹登录页。
    /// 上一版把这份文件整个删掉，于是桌面端用户卡在「登录 ChatGPT」页（真机报障）。
    #[test]
    fn apply_auth_writes_placeholder_only_when_there_is_nothing_to_preserve() {
        let d = temp_dir("write_ph");
        let auth = d.join("auth.json");

        // ① 文件不存在 → 写占位符
        let note = apply_auth_at(&auth).unwrap().expect("无凭据时必须写");
        assert!(note.contains("登录页"), "文案要说明这是干什么用的：{note}");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
        assert_eq!(
            v["OPENAI_API_KEY"].as_str(),
            Some(super::CODEX_AUTH_PLACEHOLDER)
        );
        assert!(
            !v["OPENAI_API_KEY"].as_str().unwrap().is_empty(),
            "空值过不了那道门（实测 authMethod 返 null）"
        );

        // ② 幂等：已经是我们的占位符 → 不再写、也不把「已接入态」拷进 .bak。
        //    这一支不可省：拷进去之后点还原拿回来的就是那个假 key。
        assert_eq!(apply_auth_at(&auth).unwrap(), None, "第二次必须短路");

        // ③ 旧版本留下的历史占位值同样算「我们的」（识别面比写入面宽）。
        write(&auth, r#"{"OPENAI_API_KEY":"synaroute-proxy"}"#);
        assert_eq!(
            apply_auth_at(&auth).unwrap(),
            None,
            "历史占位值也要认得出来，否则会被当成用户真凭据"
        );

        std::fs::remove_dir_all(&d).ok();
    }

    /// 那句 401 回显里**可见的两截**必须自己解释自己。
    ///
    /// OpenAI 只回显前 8 位 + 后 4~5 位（实测三种长度都是这个规则）。旧值 `synaroute-proxy`
    /// 显示成 `synarout***roxy` —— 看着就像个真 key，于是用户被那句
    /// 「You can find your API key at platform.openai.com」引去查自己的密钥（方向完全相反）。
    /// 现值显示成 `SEE-SYNA***ROUTE`。
    ///
    /// 那句报错是这条链路上**唯一必然到达用户眼前**的文本，前 8 位是我们能控制的全部带宽。
    #[test]
    fn the_visible_part_of_the_placeholder_points_at_synaroute() {
        let ph = super::CODEX_AUTH_PLACEHOLDER;
        let head = &ph[..8];
        let tail = &ph[ph.len() - 5..];
        assert_eq!(head, "SEE-SYNA", "前 8 位是回显里可见的那截，必须指向 SynaRoute");
        assert_eq!(tail, "ROUTE");
        assert!(ph.len() > 20, "太短会让中间的打码段藏不住东西，反而像个真 key");
        // 绝不能长得像真 key：真 key 以 `sk-` 开头。
        assert!(!ph.starts_with("sk-"), "不许伪装成真 key 的形态");
        // 历史值必须仍在识别清单里，否则老用户盘上那份认不出来。
        assert!(is_our_placeholder_value("synaroute-proxy"));
        assert!(is_our_placeholder_value(ph));
        assert!(!is_our_placeholder_value("sk-userrealkey123"));
    }

    /// 占位符只有一份事实来源：Codex 的 bearer、桌面端 gateway、Claude CLI 的
    /// `ANTHROPIC_AUTH_TOKEN` 必须全部引用同一个常量。
    ///
    /// 抄成三份的后果不是「不一致」这么轻 —— 任何「这个 token 是不是我们写的」的判断都会
    /// 答「不是我们的」，清理与告警一起静默失效（fail-open）。编译器管不到字面量这条缝。
    ///
    /// ⚠️ **判据必须放过常量自己的定义行**，否则这个门会永远报自己 —— 与
    /// CLAUDE.md 里「密钥扫描门不再报自己」是同一个坑（第一版就踩了）。
    #[test]
    fn placeholder_has_a_single_source_of_truth() {
        // 本仓的 .rs 是 CRLF（见 .gitattributes）。`include_str!` 给的是原始字节，
        // 不先归一行尾，下面那个 split 找不到锚点 → `prod` 变成整份文件、把测试段里的
        // 字面量也算成违规。**判据自己失效的方向必须是响亮的**：所以下面还有一条反向断言。
        // **两个文件都要查**。只查 tools.rs 会留下最要紧的那条缝：占位符的全部使用点
        // 现在都在 codex.rs 里，漏掉它等于让门在真正会出问题的地方失效。
        let files = [
            ("tools.rs", include_str!("../tools.rs").replace("\r\n", "\n")),
            ("tools/codex.rs", include_str!("codex.rs").replace("\r\n", "\n")),
        ];
        let mut checked = 0usize;
        for (name, src) in &files {
            // 只看生产段（尾部 `#[cfg(test)] mod tests` 之前），与棘轮脚本同一口径。
            // 用 rfind 取**最后**一处：文件中间还有若干 `#[cfg(test)]` 单项（如 REAL_CLEANUP_CALLS）。
            let cut = src
                .rfind("\n#[cfg(test)]\nmod tests")
                .unwrap_or_else(|| panic!("{name} 尾部应有 `#[cfg(test)] mod tests`"));
            let prod = &src[..cut];
            let offenders: Vec<&str> = prod
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    !t.starts_with("//") && t.contains("\"synaroute-proxy\"")
                })
                // 放过两处合法的：常量自己的定义，以及**历史占位值清单**。
                // 后者必须留着字面量 —— 它的语义正是「盘上可能出现过的旧值」，
                // 改用常量就等于让它跟着现值走，那样老用户盘上那份就再也认不出来
                // （识别面必须比写入面宽，见 `LEGACY_CODEX_AUTH_PLACEHOLDERS`）。
                .filter(|l| {
                    !l.contains("const PROXY_PLACEHOLDER")
                        && !l.contains("LEGACY_CODEX_AUTH_PLACEHOLDERS")
                })
                .collect();
            assert!(
                offenders.is_empty(),
                "{name} 生产段里仍有裸的 \"synaroute-proxy\" 字面量，\
                 必须改用 PROXY_PLACEHOLDER / LEGACY_CODEX_AUTH_PLACEHOLDERS：{offenders:?}"
            );
            checked += 1;
        }
        // 反向判据：门不能因为「压根没找到那个字符串」而恒绿（同 `invoke-command-must-exist`
        // 那条教训 —— 解析到 0 个候选时要主动判失败）。
        assert_eq!(checked, 2, "两个文件都要真的被查过");
        assert!(
            files[0].1.contains("const PROXY_PLACEHOLDER"),
            "常量定义都找不到，说明这条判据在空转"
        );
        assert!(
            files[1].1.contains("LEGACY_CODEX_AUTH_PLACEHOLDERS: &[&str]"),
            "历史占位值清单找不到了 —— 老用户盘上那份假 key 会认不出来"
        );
        assert_eq!(PROXY_PLACEHOLDER, "synaroute-proxy");
    }
}
