//! 桌面悬浮窗（第⑥批）。
//!
//! ## 显示条件是**两个条件的与**
//!
//! 1. 用户在设置里开了 `floating_widget_enabled`（默认关）；
//! 2. 主窗口当前**已隐藏到托盘**。
//!
//! 缺一不可。第 2 条是用户明确要求的语义：主窗口在前台时悬浮窗只会挡事。
//! 这也意味着显示/隐藏的触发点不在设置页，而在主窗口的 `CloseRequested`
//! （→ 藏到托盘）与托盘「显示主窗口」（← 从托盘唤起）两处，各切换一次。
//!
//! ## 为什么必须是独立窗口
//!
//! 上一轮的 `QuickPanel` 是主窗口内的 React 组件。主窗口一旦 `hide()`，
//! 它连同整个 WebView 一起消失 —— 「最小化到托盘后才出现」用页面内组件
//! 在物理上就做不到。故这里新建一个 Tauri 窗口，加载同一份前端但走
//! `?floating=1` 路由分支。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 悬浮窗的窗口标签。前端靠 URL 上的 `floating=1` 判断该渲染哪个界面。
pub const FLOATING_LABEL: &str = "floating";

/// 悬浮球尺寸。**正方形**：前端画的是个圆，宽高不等会画成椭圆。
///
/// 原先是 300×208 的矩形卡片，铺开四行明细 —— 那个形状在桌面上很挡事，
/// 而它想说的话（三端状态 / 今日花费）只在用户主动看一眼时才需要。
/// 现在收成一颗球：常态只显示图标 + 运行数，明细改为**悬停展开**。
const FLOATING_BALL: f64 = 64.0;

/// 悬停展开后的窗口尺寸。展开态要装下原来那几行明细，故宽高都放开。
///
/// **窗口必须先够大**：WebView 画不出窗口之外的东西，若窗口维持 64×64，
/// 前端那块展开面板会被窗口边界直接裁掉（表现为「悬停没反应」）。
/// 故展开/收起是前端与后端**一起**改的 —— 前端换布局、后端换窗口尺寸。
const FLOATING_PANEL_W: f64 = 300.0;
const FLOATING_PANEL_H: f64 = 208.0;

/// 算「主屏右下角」的逻辑坐标。
///
/// **不接受窗口参数**：坐标必须能在**建窗之前**算出来，故只依赖 `AppHandle`
/// 拿到的显示器信息，与任何窗口的状态无关。
///
/// 这一条是本文件最关键的教训。原实现是 `w.current_monitor()` ——
/// 那个 API 的语义是「窗口当前所在的显示器」，而窗口以 `visible(false)` 建出来、
/// 还没显示时它返回 `Ok(None)`。原代码写的是
/// `let Ok(Some(m)) = w.current_monitor() else { return }`，于是**静默 return、
/// 位置一次都没设过**，窗口停在 Tauri 给无边框窗的默认位置（实测落在屏幕可见区之外）。
///
/// 而这条路径不产生任何错误：日志里 `show()` 成功、建窗成功、判据全对，
/// 用户看到的却是「桌面上什么都没有」—— 全屏截图才抓到它压根不在屏幕上。
///
/// 返回 `None` 表示连显示器信息都拿不到，由调用方决定兜底。
fn bottom_right_position(app: &AppHandle) -> Option<tauri::LogicalPosition<f64>> {
    // primary_monitor 不依赖任何窗口；拿不到主屏时退而用第一个可用显示器
    let monitor = app
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| app.available_monitors().ok()?.into_iter().next())?;

    let size = monitor.size();
    let scale = monitor.scale_factor();
    // 物理像素 → 逻辑像素后再算，否则高 DPI 屏上会偏出屏幕
    let sw = size.width as f64 / scale;
    let sh = size.height as f64 / scale;
    // 距右下各留 24 逻辑像素；再往上抬 48 躲开 Windows 任务栏的典型高度。
    //
    // 按**展开态**的尺寸留位置，而不是按球的 64×64：展开是往右下角那侧长出来的，
    // 若按球定位，贴边放的球一展开就有一半长到屏幕外去了。
    Some(tauri::LogicalPosition::new(
        (sw - FLOATING_PANEL_W - 24.0).max(0.0),
        (sh - FLOATING_PANEL_H - 24.0 - 48.0).max(0.0),
    ))
}

/// 建窗（已存在则复用）。**不负责显示**，显示由 [`sync_visibility`] 统一决定。
///
/// 位置在**建窗时**就通过 `.position()` 定下来，不再依赖建成之后的 `set_position`
/// （理由见 [`bottom_right_position`]）。
///
/// ## 绝不要在窗口事件回调里调用它
///
/// 这个函数**必须**在 `setup`（应用启动）阶段调用，见 [`preload`]。
///
/// 为什么：Tauri 里创建窗口、查询窗口属性都要往事件循环线程发消息并**等回复**。
/// 而 `on_window_event`（如 `CloseRequested`）的回调本身就跑在事件循环线程上 ——
/// 在里面同步建窗就是等自己，实测表现为 `show()` 返回 Ok、随后
/// `outer_position()` / `outer_size()` 全部读取失败（日志里那条「位置/尺寸读取失败」）。
/// 窗口对象存在但从未真正完成初始化，屏幕上什么也没有。
///
/// `pinned` = 是否置顶（用户设置项）。**建窗时就定下来**，此后由
/// `set_floating_pinned` 改。
fn ensure_window(app: &AppHandle, pinned: bool) -> tauri::Result<tauri::WebviewWindow> {
    if let Some(w) = app.get_webview_window(FLOATING_LABEL) {
        return Ok(w);
    }

    let mut b = WebviewWindowBuilder::new(
        app,
        FLOATING_LABEL,
        // 查询串而非 hash：hash 变化不会让 WebView 重新求值初始 URL，
        // 而这里需要前端在**首次加载**时就知道自己是悬浮窗。
        // （Tauri 的 asset handler 会 `split(&['?','#'])` 去掉查询串再查资源，
        //  故 `/index.html` 能正常命中 —— 已核对 protocol/tauri.rs:146。）
        WebviewUrl::App("index.html?floating=1".into()),
    )
    .title("SynaRoute")
    // 建出来就是球的尺寸；悬停展开时由 `set_floating_expanded` 改成面板尺寸。
    .inner_size(FLOATING_BALL, FLOATING_BALL)
    .resizable(false)
    // 无边框 + 不进任务栏：这是「悬浮小球」而不是第二个主窗口。
    .decorations(false)
    .skip_taskbar(true)
    // 透明 + 关阴影：圆形的关键。窗口本身是方的，圆是前端用 border-radius 画的 ——
    // 不透明的话四角会露出方形底色（一颗球外面套个方框），而系统阴影是按**窗口矩形**
    // 投的，留着就会在球周围投出一个方形影子。两者必须成对，只改一个都还是方的。
    .transparent(true)
    .shadow(false)
    // **不再无条件置顶**。置顶是「用别的软件时被它挡着」的直接原因，
    // 现在由用户在设置里决定（`floating_widget_always_on_top`，默认关）。
    .always_on_top(pinned)
    // 先建后显：建窗时就 visible 会让它在定位完成前先闪一下屏幕左上角。
    .visible(false);

    // 算得出来就用右下角；算不出来**也要给一个屏内的保守坐标**，
    // 绝不能什么都不设 —— 那正是上一版的 bug。
    let pos = bottom_right_position(app).unwrap_or(tauri::LogicalPosition::new(80.0, 80.0));
    b = b.position(pos.x, pos.y);

    b.build()
}

/// 启动阶段预建悬浮窗（隐藏态）。**只应由 `setup` 调用一次。**
///
/// 为什么要预建：见 [`ensure_window`] 的「绝不要在窗口事件回调里调用它」。
/// 悬浮窗的显示时机是主窗口 `CloseRequested`（藏到托盘那一刻），而那是个事件回调 ——
/// 在里面建窗会死等事件循环。故把「建」提前到启动、只把「显/隐」留给回调。
///
/// 代价是即使用户从未开启悬浮窗，也多一个隐藏的 WebView 常驻。这笔账划算：
/// 换来的是显示路径上只剩 `show()` 这一个轻量调用，不再有任何可能卡住事件循环的操作。
/// 而用户关掉开关时 [`destroy`] 会把它彻底销毁，不想要的人也不会长期背着它。
///
/// 失败只落日志、不上抛：悬浮窗建不起来不该阻止应用启动。
///
/// 结果写进**事件日志**而非只 `tracing::warn!` —— 打包后没有控制台可看，
/// 而「预建成功没成功」正是排障时最需要的那一位信息（实测吃过这个亏：
/// 屏幕上没窗口，而日志里只有 `sync_visibility` 的记录，无从判断窗口到底建起来没）。
pub fn preload(app: &AppHandle, pinned: bool) {
    let outcome = match ensure_window(app, pinned) {
        Ok(_) => "预建悬浮窗成功（隐藏态待命）".to_string(),
        Err(e) => {
            tracing::warn!("预建悬浮窗失败（开关打开时会重试）: {e}");
            format!("预建悬浮窗失败: {e}")
        }
    };
    if let Some(state) = app.try_state::<crate::AppState>() {
        state.store.append_event(
            crate::model::CategoryType::ClaudeCli,
            "config",
            None,
            &outcome,
        );
    }
}

/// 悬浮窗的运行时诊断（供 IPC 命令调用）。
///
/// **必须从命令线程调用，不能从窗口事件回调调**：里面的 `is_visible()` /
/// `outer_position()` 都要往事件循环发消息并等回复，在事件循环线程上调用会等自己
/// （这正是上一版日志里「位置/尺寸读取失败」的来源）。命令线程上它们是可靠的。
pub fn diagnose(app: &AppHandle) -> String {
    let Some(w) = app.get_webview_window(FLOATING_LABEL) else {
        return "悬浮窗：窗口对象不存在（预建失败或已被销毁）".to_string();
    };
    let vis = match w.is_visible() {
        Ok(v) => v.to_string(),
        Err(e) => format!("读取失败({e})"),
    };
    let pos = match w.outer_position() {
        Ok(p) => format!("({},{})", p.x, p.y),
        Err(e) => format!("读取失败({e})"),
    };
    let size = match w.outer_size() {
        Ok(s) => format!("{}x{}", s.width, s.height),
        Err(e) => format!("读取失败({e})"),
    };
    // WebView 的当前 URL —— 若它是空的或不含 floating=1，说明前端根本没加载对
    let url = match w.url() {
        Ok(u) => u.to_string(),
        Err(e) => format!("读取失败({e})"),
    };
    let target = bottom_right_position(app)
        .map(|p| format!("({:.0},{:.0})", p.x, p.y))
        .unwrap_or_else(|| "拿不到显示器信息".to_string());

    format!(
        "悬浮窗诊断：存在=是 可见={vis} 实际位置={pos} 尺寸={size} 目标位置={target} URL={url}"
    )
}

/// 按「开关 × 主窗口是否隐藏」决定悬浮窗的显隐。
///
/// 幂等：可以在任何状态变化点无脑调一次，它自己算当前该是什么样。
/// 这是刻意的 —— 分散在各处的 `show()`/`hide()` 必然漏一个分支，
/// 而漏掉的那个会表现为「悬浮窗关不掉」或「开了却不出现」。
///
/// ## 为什么把判据写进**事件日志**而不只是 tracing
///
/// 这个函数的所有失败路径都是静默的：`enabled=false`、主窗口没隐藏、建窗报错，
/// 三种情况用户看到的现象完全一样 —— 「开关开了但桌面上没东西」。
/// 而 `tracing::warn!` 只进 stderr（打包后没有控制台可看），排障时等于没有。
///
/// 故这里把 `enabled` / `main_hidden` / `should_show` 三个判据 + 建窗结果都记进
/// 运行日志，用户复现一次就能在「运行日志」页里看到到底卡在哪一步。
/// 这不是调试残留，是这类「多条件与、任一不满足都表现为无反应」的功能必须有的可观测性。
///
/// `pinned` = 用户是否要它置顶（`floating_widget_always_on_top`）。
pub fn sync_visibility(app: &AppHandle, enabled: bool, pinned: bool) {
    // 主窗口是否处于隐藏态。取不到主窗口（极早期/已退出）时按「未隐藏」处理，
    // 于是悬浮窗不显示 —— 宁可不显示，也不要在退出过程中弹个窗出来。
    let main_visible = app
        .get_webview_window("main")
        .map(|m| m.is_visible().unwrap_or(true));
    let main_hidden = main_visible.map(|v| !v).unwrap_or(false);

    let should_show = enabled && main_hidden;

    // 把判据落进日志。`main_visible` 用 Option 原样表达「拿不到主窗口」这第三种情况，
    // 它与「窗口可见」的后果相同（都不显示悬浮窗）但原因完全不同。
    let verdict = |outcome: &str| {
        if let Some(state) = app.try_state::<crate::AppState>() {
            state.store.append_event(
                crate::model::CategoryType::ClaudeCli,
                "config",
                None,
                &format!(
                    "悬浮窗同步：开关={} 主窗口={} → {}",
                    if enabled { "开" } else { "关" },
                    match main_visible {
                        Some(true) => "可见",
                        Some(false) => "已隐藏",
                        None => "取不到",
                    },
                    outcome
                ),
            );
        }
    };

    if !should_show {
        let had = app.get_webview_window(FLOATING_LABEL).is_some();
        if let Some(w) = app.get_webview_window(FLOATING_LABEL) {
            let _ = w.hide();
        }
        // 只在「本来显示着、现在收起」时记一条；两个条件本就不满足时不记，
        // 免得每次唤起主窗口都写一条噪音。
        if had {
            verdict("隐藏悬浮窗");
        }
        return;
    }

    match ensure_window(app, pinned) {
        Ok(w) => {
            // 定位放在 show() **之前**：先摆好再显示，避免它先在旧位置闪一下。
            //
            // 复用已有窗口时（预建的那个、或用户拖动过的）也要重新摆回右下角 ——
            // 多屏切换、分辨率变化后原坐标可能已在屏外。
            //
            // `bottom_right_position` 只读显示器信息、不查窗口状态，故在事件回调里安全。
            let placed = bottom_right_position(app);
            if let Some(pos) = placed {
                let _ = w.set_position(pos);
            }
            // 每次显示都收回球形：上次可能是在展开态被隐藏的（鼠标还悬着就切走了），
            // 不收的话下次露面就是一个 300×208 的方块，而前端画的是球 —— 一个圆
            // 缩在大窗口左上角、其余是透明区。
            let _ = w.set_size(tauri::LogicalSize::new(FLOATING_BALL, FLOATING_BALL));

            let shown = w.show();
            // 置顶按用户设置重申。**不能无条件 true**：那正是「用别的软件时被它挡着」
            // 的原因。部分 Windows 版本上该属性会被后续窗口抢走，故每次显示都重设一次
            // —— 但重设的是用户选的值。
            let _ = w.set_always_on_top(pinned);

            // 记**算出来的**坐标，而不是回查 `outer_position()`。
            //
            // 这一点是上一版的教训：`outer_position()` / `outer_size()` 同样要往事件循环
            // 发消息并等回复，而本函数会在 `CloseRequested` 回调（即事件循环线程）里被调用
            // —— 于是那两个读取在关键路径上恒失败，日志只留下「位置/尺寸读取失败」，
            // 反而掩盖了真正要观测的坐标。
            //
            // 记算出的值虽然少了「实际生效位置」这层确认，但它不会失败，
            // 且足以判断「坐标是否落在屏内」——那才是排障要的信息。
            let geom = match placed {
                Some(p) => format!(
                    "目标位置=({:.0},{:.0}) 尺寸={:.0}x{:.0} 置顶={}",
                    p.x, p.y, FLOATING_BALL, FLOATING_BALL, pinned
                ),
                None => "拿不到显示器信息，已用兜底坐标".to_string(),
            };

            match shown {
                Ok(()) => verdict(&format!("已显示悬浮窗 · {geom}")),
                Err(e) => {
                    tracing::warn!("悬浮窗 show() 失败: {e}");
                    verdict(&format!("显示失败: {e} · {geom}"));
                }
            }
        }
        Err(e) => {
            tracing::warn!("创建悬浮窗失败: {e}");
            verdict(&format!("创建窗口失败: {e}"));
        }
    }
}

/// 关闭并销毁悬浮窗（用户关掉开关时调）。
///
/// 用 `close` 而不是 `hide`：开关关掉后它不该再占着一个 WebView 进程
/// （每个 WebView 都是实打实的内存与 GPU 开销）。下次开启时重建。
pub fn destroy(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(FLOATING_LABEL) {
        let _ = w.close();
    }
}

/// 悬停展开 / 移出收起：改窗口尺寸。
///
/// **为什么非要动窗口尺寸**：WebView 画不出窗口以外的东西。若窗口固定 64×64，
/// 前端那块展开面板会被窗口边界直接裁掉，用户看到的是「悬停没反应」——
/// 而 CSS 层面明明已经展开了。这是无边框小窗最容易踩的一条。
///
/// 展开方向是**左上**（`set_position` 往左上挪）而不是右下：悬浮球停在屏幕右下角，
/// 往右下长会直接长到屏幕外面去。故展开时把窗口原点左移/上移相应的差值，
/// 球本身在屏幕上的视觉位置保持不动。
///
/// 由前端的 `mouseenter` / `mouseleave` 调用。失败只记 tracing：
/// 展开不了顶多是看不到明细，不值得打断用户。
pub fn set_expanded(app: &AppHandle, expanded: bool) {
    let Some(w) = app.get_webview_window(FLOATING_LABEL) else {
        return;
    };

    // 先拿当前位置，再算新原点。拿不到就只改尺寸 —— 位置会显得跳一下，
    // 但仍比「展开被裁掉」好。
    let cur = w.outer_position().ok();
    let scale = w.scale_factor().unwrap_or(1.0);

    let (tw, th) = if expanded {
        (FLOATING_PANEL_W, FLOATING_PANEL_H)
    } else {
        (FLOATING_BALL, FLOATING_BALL)
    };

    if let Err(e) = w.set_size(tauri::LogicalSize::new(tw, th)) {
        tracing::warn!("悬浮窗改尺寸失败: {e}");
        return;
    }

    // 让球的**右下角**保持不动：展开时原点往左上退 (panel - ball)，收起时反向。
    if let Some(p) = cur {
        let delta = FLOATING_PANEL_W - FLOATING_BALL;
        let delta_h = FLOATING_PANEL_H - FLOATING_BALL;
        let lx = p.x as f64 / scale;
        let ly = p.y as f64 / scale;
        let (nx, ny) = if expanded {
            (lx - delta, ly - delta_h)
        } else {
            (lx + delta, ly + delta_h)
        };
        // 钳到 0：多屏负坐标下这个夹取会把窗口拉回主屏，但比长到屏外不可见更可控
        let _ = w.set_position(tauri::LogicalPosition::new(nx.max(0.0), ny.max(0.0)));
    }
}

/// 改置顶属性（用户在设置里切「悬浮球置顶」时调）。
///
/// 即时生效、不需要重建窗口 —— 重建会让球闪一下并跳回右下角，
/// 而用户可能刚把它拖到别处。
pub fn set_pinned(app: &AppHandle, pinned: bool) {
    if let Some(w) = app.get_webview_window(FLOATING_LABEL) {
        if let Err(e) = w.set_always_on_top(pinned) {
            tracing::warn!("悬浮窗置顶设置失败: {e}");
        }
    }
}
