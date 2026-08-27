//! 托盘图标与 tooltip 的**派生**逻辑。
//!
//! 从 lib.rs 抽出来的：这三段都是「从运行时already有的东西现场算出另一个东西」
//! （打包图标 → 灰度停止态图标；运行状态 + 选定模型 → tooltip 文本），
//! 与 lib.rs 那些 IPC 命令注册没有关系，而 lib.rs 的棘轮余量为 0。
//!
//! ⚠️ 平台差异是这里的核心判据：macOS 的菜单栏图标是 template（系统按深浅色填色，
//! 拿不到颜色也拿不到透明度），所以「彩色=运行 / 灰度=停止」那套在 mac 上完全失效 ——
//! 故灰度派生整段 `#[cfg(not(target_os = "macos"))]`，而运行状态必须同时写进 tooltip
//! 文本，否则 mac 用户无处可看。

use crate::model::CategoryType;
use crate::AppState;
use tauri::Manager;
/// 托盘 tooltip：附带当前 Codex 选定模型，让用户悬停即知当前用哪个（无需展开菜单）。
pub(crate) fn tray_tooltip(app: &tauri::AppHandle) -> String {
    let state = app.state::<AppState>();
    let settings = state.store.get_settings();

    // 运行状态必须在 tooltip 里：macOS 的菜单栏图标是 template（单色、由系统按深浅色填色），
    // 拿不到颜色也拿不到透明度，`stopped_tray_icon` 那套「彩色=运行 / 灰度=停止」在 mac 上
    // 完全失效（见 `apply_tray_icon`）。若状态只由图标表达，mac 用户将无处可看。
    //
    // Windows 上图标仍然分两态，这里的文本是冗余的 —— 但冗余无害，且两平台同一份文本
    // 省掉一处平台分叉。
    let running: Vec<&str> = CategoryType::ALL
        .iter()
        .filter(|c| state.proxy.is_running(**c))
        .map(|c| c.display_name())
        .collect();
    let status = if running.is_empty() {
        "已停止".to_string()
    } else {
        format!("运行中: {}", running.join(" / "))
    };

    match settings.active_models.get(&CategoryType::Codex) {
        Some(m) if !m.trim().is_empty() => format!("SynaRoute · {status} · Codex: {m}"),
        _ => format!("SynaRoute · {status}"),
    }
}

/// 把 RGBA8 像素就地灰度化并降不透明度，用于派生「已停止」态托盘图标。
///
/// 返回是否成功：长度不是 4 的整数倍即判失败并**不做任何修改**（宁可两态同图标，
/// 也不要因错位画出花屏）。
///
/// 灰度用感知亮度权重（0.299/0.587/0.114）而非三通道均值——均值会让偏蓝的图标看起来
/// 比实际更亮。alpha 乘 0.55 让它明显「淡下去」，即使用户系统主题下灰度对比不明显，
/// 也能靠透明度分辨。
///
/// 仅非 macOS 使用（mac 上托盘图标是 template，见 `apply_tray_icon`）。
#[cfg(not(target_os = "macos"))]
fn desaturate_rgba_in_place(rgba: &mut [u8]) -> bool {
    if rgba.is_empty() || rgba.len() % 4 != 0 {
        return false;
    }
    for px in rgba.chunks_exact_mut(4) {
        let lum = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32)
            .round()
            .clamp(0.0, 255.0) as u8;
        px[0] = lum;
        px[1] = lum;
        px[2] = lum;
        px[3] = (px[3] as f32 * 0.55).round().clamp(0.0, 255.0) as u8;
    }
    true
}

/// 「已停止」状态的托盘图标：由打包图标灰度化 + 降不透明度派生。
///
/// **为什么派生而不是加一份 png 资源**：图标是 `tauri.conf.json` 里配的、打包时嵌入的，
/// 日后换图标只会换那一份。若另存一张「灰色版」，换图标时必然忘记同步，托盘就会出现
/// 「运行时是新图标、停止时是旧图标」的割裂。从运行时拿到的 RGBA 现场派生，永远自动对齐。
///
/// 结果缓存（`OnceLock`）：托盘每次重建都会取一次，而源图标在进程内恒定。
///
/// macOS 不用它：那边图标走 template（系统按深浅色填色），派生灰度版渲染结果与原图无异。
#[cfg(not(target_os = "macos"))]
pub(crate) fn stopped_tray_icon(app: &tauri::AppHandle) -> Option<tauri::image::Image<'static>> {
    static CACHED: std::sync::OnceLock<Option<(Vec<u8>, u32, u32)>> = std::sync::OnceLock::new();
    let cached = CACHED.get_or_init(|| {
        let src = app.default_window_icon()?;
        let (w, h) = (src.width(), src.height());
        let mut rgba = src.rgba().to_vec();
        if !desaturate_rgba_in_place(&mut rgba) {
            return None;
        }
        Some((rgba, w, h))
    });
    cached
        .as_ref()
        .map(|(rgba, w, h)| tauri::image::Image::new_owned(rgba.clone(), *w, *h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「已停止」图标的派生：必须真的变灰、真的变淡，且不改尺寸。
    /// 直接测像素变换本身（不依赖 AppHandle，单测里拿不到真实图标）。
    ///
    /// 随被测函数一起门控：mac 上托盘图标走 template，不存在灰度派生这条路。
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn stopped_icon_derivation_greys_out_and_fades() {
        // 一个 2×1 的图：纯红不透明 + 纯蓝半透明。
        let mut rgba = vec![255u8, 0, 0, 255, 0, 0, 255, 128];
        assert!(desaturate_rgba_in_place(&mut rgba));

        // 像素 1：红 → 亮度 0.299*255 ≈ 76，三通道相等
        assert_eq!(&rgba[0..3], &[76, 76, 76], "必须灰度化为三通道相等");
        assert_eq!(rgba[3], 140, "alpha 应乘 0.55（255*0.55≈140）");
        // 像素 2：蓝 → 亮度 0.114*255 ≈ 29
        assert_eq!(&rgba[4..7], &[29, 29, 29]);
        assert_eq!(rgba[7], 70, "半透明像素也按同比例变淡（128*0.55≈70）");
    }

    /// 像素数据长度非法时必须原样返回、不做部分修改——宁可两态同图标，也不要画出花屏。
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn stopped_icon_derivation_rejects_malformed_pixel_data() {
        let mut odd = vec![1u8, 2, 3]; // 不是 4 的整数倍
        assert!(!desaturate_rgba_in_place(&mut odd));
        assert_eq!(odd, vec![1, 2, 3], "失败时不得留下半改状态");

        let mut empty: Vec<u8> = vec![];
        assert!(!desaturate_rgba_in_place(&mut empty));
    }
}
