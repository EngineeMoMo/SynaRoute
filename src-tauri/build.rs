//! 构建脚本。
//!
//! 除了 `tauri_build::build()`，这里还补了一件 tauri-build **没做**的事：
//! 为图标文件发出 `cargo:rerun-if-changed`。
//!
//! ## 为什么必须自己补（实测踩到）
//!
//! Windows 上 `tauri-build` 用 `res.set_icon_with_id("icons/icon.ico", "32512")`
//! 把图标编译进 exe 的资源段（tauri-build-2.6.3/src/lib.rs:669）。但通篇搜过它发出的
//! `cargo:rerun-if-changed` —— 覆盖了 config、capabilities、permissions、resources、
//! gradle，**唯独没有图标**。
//!
//! 后果：换了图标但 `build.rs` 的输入没变（Cargo.toml、源码都没动），Cargo 判定
//! build script 无需重跑，于是资源段里一直是**上次编译时**那份旧图标。
//! 而前端资源走的是另一条路（`dist/` 打包），所以会出现「新界面 + 旧图标」这种
//! 看起来很矛盾的状态 —— 实测就是这么发现的：exe 里 chunk 名是新的，
//! 但 `ExtractAssociatedIcon` 导出来还是上一版图形。
//!
//! 这类失效不报错、不警告，只能靠肉眼看图标。故在此显式声明依赖。

fn main() {
    // 与 tauri.conf.json 的 bundle.icon 列表保持一致。
    //
    // `icon.ico` 是 Windows 资源段真正用的那个（tauri-build 只挑第一个 `.ico`）；
    // 其余几个进安装包与各平台 bundle。全部声明是刻意的：任何一个变了都该重新构建，
    // 免得下次换图标时又只改了其中一个、而另一个悄悄留着旧图形。
    for icon in [
        "icons/icon.ico",
        "icons/icon.png",
        "icons/32x32.png",
        "icons/128x128.png",
        "icons/128x128@2x.png",
    ] {
        println!("cargo:rerun-if-changed={icon}");
    }

    tauri_build::build()
}
