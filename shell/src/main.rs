//! LightCraft 的 OTools 插件外壳。
//!
//! 上游 `apps/lightcraft-web` 的浏览器宿主（OPFS/IndexedDB 持久化、渲染 worker、
//! 备份/恢复、拖放导入）被逐字复用——`src/{backend,backup,bench,files,safety,store,
//! wire,workers}.rs` 是上游在 upstream.pin 那个 commit 的原样副本；`src/web.rs` 是
//! 它 web.rs 的改写版，只动了「宿主边界」：
//!
//! 1. 导入照片弹 OTools 宿主的打开对话框（`src/otools.rs` → `window.lightcraftOtools`）
//! 2. 导出交给宿主的另存对话框写真实磁盘
//! 3. 暗黑主题钉死为默认（`system_theme` 恒报 Dark）
//! 4. 界面语言跟随宿主 locale（`otools-locale-changed` 事件）
//!
//! 上游 `crates/*` 一律不改：它们按 upstream.pin 的 commit 拉取，保持原样才能干净地跟上上游。
//! 详细约定见 AGENTS.md。

mod backup;
mod bench;
mod files;
mod store;
mod wire;

#[cfg(target_arch = "wasm32")]
mod backend;
#[cfg(target_arch = "wasm32")]
mod otools;
#[cfg(target_arch = "wasm32")]
mod safety;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
mod workers;

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("otools-lightcraft 只在浏览器里运行：在 shell/ 目录用 `trunk build --release` 构建");
}
