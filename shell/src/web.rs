//! OTools 插件宿主：改自上游 `apps/lightcraft-web/src/web.rs`（upstream.pin 的 commit）。
//!
//! 与上游网页版的差异只有四处，其余（OPFS/IndexedDB 持久化、渲染 worker、备份/恢复、
//! 拖放导入、通知横幅、`?store/?workers/?reset` 参数）全部原样保留：
//!
//! 1. **导入**：`Services.pick_files` 弹 OTools 宿主的打开对话框（`window.lightcraftOtools`），
//!    读到的字节仍走上游的 `store_file`（内容寻址存进浏览器存储）；
//! 2. **导出**：`Services.write` 不再触发浏览器下载，而是交给宿主的另存对话框写真实磁盘；
//! 3. **主题**：`Services.system_theme` 恒报 Dark —— Appearance 默认就是 Dark（上游默认），
//!    这里把 Auto 也钉在 Dark 上，宿主浅色不会把界面带成浅色（用户仍可在 Settings 里改）；
//! 4. **语言**：跟随宿主 locale（`window.__OTOOLS_LOCALE__`），宿主切换语言时下一帧
//!    重写 `ui.language`（上游网页版没有宿主语言这个概念）。
//!
//! 外部链接（Discord / getartcraft.com / 上游 GitHub）被 `open_url` 静默吞掉：插件里点了
//! 不该把用户拽出去。上游浏览器宿主的 `<input type=file>` 选择器与「下载」导出已删除。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::Source;
use lightcraft_engine::library::LibraryStores;
use lightcraft_ui_egui::{LightcraftApp, Services, UiState};
use serde_json::json;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::backend::Backend;
use crate::backup::{ACTIVE_LIBRARY, DEFAULT_LIBRARY_DIR, valid_library_dir};
use crate::bench::Bench;
use crate::files::{Files, FlushOp, LIBRARY_FILES};
use crate::safety::{self, notice};
use crate::store::{Originals, content_hash, storage_key};
use crate::wire::ThumbIndex;
use crate::workers::{THUMB_INDEX, Workers};

/// 「文件 ▸ Add from Device」等入口的打开对话框过滤（与上游网页版 ACCEPT 一致）。
const PHOTO_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "nef", "arw", "psd", "jxl", "gif", "bmp"];
/// After a failed save, wait this long before trying again (ms).
const SAVE_RETRY_MS: f64 = 2000.0;

/// How long a notice stays on screen (s).
const NOTICE_SECS: f64 = 8.0;

/// How often view state, UI prefs and the thumbnail index are saved (ms).
const SAVE_EVERY_MS: f64 = 1000.0;

thread_local! {
    /// 宿主语言切换后，下一帧把宿主 locale 写进 `ui.language`（首帧也置一次）。
    static NEEDS_LANG_SYNC: Cell<bool> = const { Cell::new(false) };
    static DID_INIT_LANG: Cell<bool> = const { Cell::new(false) };
    /// The egui context, for repaints from async tasks.
    static CTX: std::cell::RefCell<Option<egui::Context>> = const { std::cell::RefCell::new(None) };
}

/// The page's window (`None` only outside a browser page, e.g. in a worker).
fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// Milliseconds since navigation start.
fn perf_now() -> f64 {
    window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or(0.0)
}

fn set_status(text: &str) {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_status")) {
        el.set_text_content(Some(text));
    }
}

/// Storage key of library file `name` in library folder `dir`.
fn library_key(dir: &str, name: &str) -> String {
    format!("{dir}/{name}")
}

/// Repaint the app (notices from async tasks).
pub(crate) fn request_repaint() {
    CTX.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.request_repaint();
        }
    });
}

/// URL options (`?bench&store=idb&workers=2&reset`).
struct Options {
    bench: bool,
    /// "opfs" (default: OPFS, falling back to IndexedDB), "idb" or "memory".
    store: String,
    workers: Option<usize>,
    reset: bool,
}

impl Options {
    fn from_url() -> Options {
        let q = window().and_then(|w| w.location().search().ok()).unwrap_or_default();
        let params: Vec<(String, String)> = q
            .trim_start_matches('?')
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect();
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        Options {
            bench: get("bench").is_some(),
            store: get("store").filter(|s| s == "idb" || s == "memory").unwrap_or_else(|| "opfs".into()),
            workers: get("workers").and_then(|v| v.parse().ok()),
            reset: get("reset").is_some(),
        }
    }
}

/// Store a picked/dropped file (bytes into browser storage, then queue its import).
/// 上游原样保留：宿主对话框选来的照片和拖进来的照片走同一条路。
async fn store_file(originals: Originals, backend: Option<Backend>, name: String, bytes: Vec<u8>, ctx: egui::Context) {
    let hash = content_hash(&bytes);
    if let Some(b) = &backend
        && let Err(e) = b.write(&storage_key(&hash), &bytes).await
    {
        // a photo whose bytes aren't stored would be gone after a reload: don't add it
        notice(format!("{name} was not added: browser storage refused it ({e}). Free up space or back up and clear old photos."));
        return;
    }
    originals.added(&name, &hash, Arc::from(bytes));
    ctx.request_repaint();
}

/// 「文件 ▸ Add from Device」：弹宿主的打开对话框，把选中的照片读进来存进浏览器存储。
/// 字节就位后 `originals` 记下待导入项，下一帧 `import_dropped` 跑 `library.import`。
fn pick_via_host(originals: Originals, backend: Option<Backend>, ctx: egui::Context) {
    let request = crate::otools::open_request("添加照片", PHOTO_EXTS);
    wasm_bindgen_futures::spawn_local(async move {
        match crate::otools::open_files(&request).await {
            Ok(files) => {
                for (name, _path, bytes) in files {
                    // 用真实文件名导入；宿主路径不进目录（内容寻址存储不认它）。
                    store_file(originals.clone(), backend.clone(), name, bytes, ctx.clone()).await;
                }
            }
            Err(e) => {
                log::error!("import: {e}");
                notice(format!("导入失败：{e}"));
            }
        }
    });
}

/// Build the browser host's `Services`: library backup/restore through browser storage,
/// asynchronous file picking, host writes for exports, and PNG encoding. Desktop-only
/// services (native pickers, reveal, model folders) stay unset.
fn services(originals: Originals, backend: Option<Backend>, files: Files, frozen: Rc<Cell<bool>>, ctx: egui::Context) -> Services {
    let (backup_backend, backup_files) = (backend.clone(), files);
    let restore_backend = backend.clone();
    Services {
        local_places: None,
        picker: None,
        pick_display_profile: None,
        screen_profile: None,
        backup_library: Some(Box::new(move |session: &mut Session| {
            let Some(b) = backup_backend.clone() else { return Err("nothing is stored in this browser session (?store=memory)".into()) };
            if let Some(why) = crate::backup::unsaved_refusal(session.unsaved()) {
                return Err(why);
            }
            // the photos' own names for the originals in the zip
            let names: std::collections::HashMap<String, String> = session
                .catalog
                .photos()
                .filter_map(|p| match &p.source {
                    Source::File { path } => crate::store::hash_of_path(path).map(|h| (h.to_string(), p.file_name.clone())),
                    _ => None,
                })
                .collect();
            let files = backup_files.clone();
            notice("Backing up the library… (the download starts when it's ready)");
            wasm_bindgen_futures::spawn_local(async move {
                match safety::backup(files, b, names).await {
                    Ok((n, bytes)) => notice(format!(
                        "Library backed up: {n} original{} ({:.1} MB). Keep the file somewhere safe.",
                        if n == 1 { "" } else { "s" },
                        bytes / 1e6
                    )),
                    Err(e) => notice(format!("Backup failed: {e}")),
                }
            });
            Ok(json!({"started": true}))
        })),
        restore_library: Some(Box::new(move |_session: &mut Session| {
            let Some(b) = restore_backend.clone() else { return Err("nothing is stored in this browser session (?store=memory)".into()) };
            let frozen = frozen.clone();
            // once the backup is in place, stop saving the old library (the page reloads)
            safety::pick_and_restore(b, move || frozen.set(true));
            Ok(json!({"started": true}))
        })),
        pick_files: Some(Box::new(move || {
            pick_via_host(originals.clone(), backend.clone(), ctx.clone());
            Vec::new() // files arrive asynchronously and are imported on a later frame
        })),
        pick_denoise_model: None,
        // Preset files: 与上游网页版一致，暂不接（宿主对话框是异步的，这里先留 None）。
        pick_preset_files: None,
        pick_tracklog: None,
        pick_lightroom_catalog: None,
        // Face models need a folder to live in; the web has none.
        pick_model_file: None,
        save_preset_file: None,
        pick_keyword_list: None,
        save_keyword_list: None,
        pick_curve_preset_files: None,
        save_curve_preset_file: None,
        pick_slideshow_templates: None,
        save_slideshow_template: None,
        pick_print_templates: None,
        save_print_template: None,
        write: Some(Box::new(|path: &str, bytes: &[u8]| {
            // 导出交给宿主的另存对话框写真实磁盘（上游网页版这里是浏览器下载）。
            // 宿主写盘是异步的，先返回 Ok 让导出流程收尾；取消/失败记在日志里。
            let name = crate::store::download_name(path).to_string();
            crate::otools::save_file(&name, bytes);
            Ok(())
        })),
        // the browser download path has no meaningful create-new file write; the control channel
        // (and therefore screenshot paths) is not available on the web
        write_new: None,
        // 宿主写盘走主线程：导出仍在前景完成（和上游网页版一致）
        write_shared: None,
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        reveal: None,
        log_file: None,
        open_with: None,
        open_all_with: None,
        check_editor: None,
        open_url: Some(Box::new(|url: &str| {
            if crate::otools::is_community_link(url) {
                Ok(())
            } else {
                Err(format!("external link blocked in OTools plugin: {url}"))
            }
        })),
        pick_folder: None,
        // 默认暗黑主题：Appearance=Auto 时也恒报 Dark（用户仍可在 Settings ▸ Appearance 里改）。
        system_theme: Some(Box::new(|_ctx: &egui::Context| Some(egui::Theme::Dark))),
    }
}

/// Write dirty library files to storage, in order, until none are left. A failure (quota,
/// storage cleared) is recorded in `files` — the catalog then reports changes as unsaved instead
/// of accepting them silently — and retried after [`SAVE_RETRY_MS`].
async fn flush(files: Files, backend: Backend, dir: String, flushing: Rc<Cell<bool>>, failed_at: Rc<Cell<f64>>) {
    'outer: loop {
        let ops = files.take_dirty();
        if ops.is_empty() {
            if files.error().is_some() {
                log::info!("lightcraft: saving works again");
            }
            files.set_error(None);
            break;
        }
        for (i, op) in ops.iter().enumerate() {
            let r = match op {
                FlushOp::Write { name, data } => backend.write(&library_key(&dir, name), data).await,
                FlushOp::Append { name, offset, data } => backend.write_at(&library_key(&dir, name), Some(*offset), data).await,
            };
            if let Err(e) = r {
                log::error!("saving {} failed: {e}", op.name());
                files.failed(&ops[i..]);
                files.set_error(Some(e));
                failed_at.set(lightcraft_ui_egui::monotonic_ms());
                break 'outer;
            }
        }
    }
    flushing.set(false);
    // the page title drops its unsaved • now, even when nothing else wakes the page
    request_repaint();
}

/// Everything loaded before the UI starts.
struct Boot {
    opts: Options,
    backend: Option<Backend>,
    files: Files,
    index: ThumbIndex,
    /// The library folder in storage (`library`, or a restored backup's).
    lib_dir: String,
    /// Library files that exist but couldn't be read (#1507): the library isn't opened and
    /// nothing is saved, so the stored copy stays as it was.
    load_error: Option<String>,
}

async fn boot(opts: Options) -> Boot {
    let t0 = perf_now();
    let backend = if opts.store == "memory" {
        None
    } else {
        match Backend::open(opts.store == "idb").await {
            Ok(b) => Some(b),
            Err(e) => {
                log::error!("no browser storage ({e}); this session won't be saved");
                None
            }
        }
    };
    let files = Files::default();
    let mut index = ThumbIndex::default();
    let mut lib_dir = DEFAULT_LIBRARY_DIR.to_string();
    let mut load_error = None;
    if let Some(b) = &backend {
        if opts.reset {
            // everything this site keeps in the browser, photos included: never without asking
            let ok = safety::confirm(
                "?reset deletes the LightCraft library stored in this browser, including every imported photo. \
                 This can't be undone. Delete it?",
            );
            if ok {
                match b.clear().await {
                    Ok(()) => log::info!("lightcraft: storage cleared (?reset)"),
                    Err(e) => log::error!("clearing storage: {e}"),
                }
            } else {
                log::info!("lightcraft: ?reset declined; storage kept");
            }
        }
        // a restored backup lives in its own folder (see `backup`)
        match b.read(ACTIVE_LIBRARY).await {
            Ok(Some(d)) => {
                let d = String::from_utf8_lossy(&d).trim().to_string();
                if valid_library_dir(&d) {
                    lib_dir = d;
                }
            }
            Ok(None) => {}
            // not knowing which library is in use, any library opened could be the wrong one
            Err(e) => load_error = Some(format!("couldn't read which library is in use from browser storage ({e})")),
        }
        if load_error.is_none() {
            let mut reads = Vec::new();
            for name in LIBRARY_FILES {
                reads.push((name, b.read(&library_key(&lib_dir, name)).await));
            }
            if let Err(e) = crate::files::preload_library(&files, reads) {
                log::error!("{e}");
                load_error = Some(e);
            }
        }
        let index_read = b.read(THUMB_INDEX).await;
        match &index_read {
            Ok(Some(j)) => index = ThumbIndex::from_json(j),
            Ok(None) => {}
            Err(e) => log::warn!("reading the thumbnail index: {e}"),
        }
        crate::backend::request_persistence(|granted| {
            if !granted {
                notice(
                    "This browser may delete LightCraft's library when it runs short of space (persistent storage wasn't granted). \
                     Keep your original photos elsewhere and use File ▸ Back Up Library… now and then.",
                );
            }
        });
        // thumbnails stored without an index entry (index saved late, or lost): delete them; not
        // when the index couldn't be read, which would make every thumbnail look unindexed
        let b2 = b.clone();
        let known: std::collections::HashSet<String> = b
            .list("thumbs")
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.ends_with(".jpg"))
            .map(|n| n.trim_end_matches(".jpg").to_string())
            .collect();
        let orphans: Vec<String> = if index_read.is_ok() { known.into_iter().filter(|k| !index.contains(k)).collect() } else { Vec::new() };
        if !orphans.is_empty() {
            wasm_bindgen_futures::spawn_local(async move {
                for k in &orphans {
                    let _ = b2.remove(&crate::wire::thumb_storage_key(k)).await;
                }
                log::info!("thumbnail cache: removed {} unindexed files", orphans.len());
            });
        }
    }
    log::info!(
        "lightcraft: storage {} opened in {:.0} ms ({} thumbnails indexed, {:.1} MB)",
        backend.as_ref().map_or("memory", |b| b.kind()),
        perf_now() - t0,
        index.len(),
        index.total() as f64 / 1e6
    );
    Boot { opts, backend, files, index, lib_dir, load_error }
}

struct WebApp {
    app: LightcraftApp,
    originals: Originals,
    files: Files,
    backend: Option<Backend>,
    workers: Option<Workers>,
    flushing: Rc<Cell<bool>>,
    /// When the last save to storage failed (ms; 0 = never).
    flush_failed_at: Rc<Cell<f64>>,
    /// Saving stopped (a restored backup is about to replace this library: the page reloads).
    frozen: Rc<Cell<bool>>,
    lib_dir: String,
    bench: Option<Bench>,
    first_frame_logged: bool,
    /// The page title last set (`document.title`), so it is written only when it changes.
    title: String,
    last_save: f64,
    ui_written: Vec<u8>,
}

/// The browser host's drop queues, taken only through the shared gate: a prompt or blocking
/// library window leaves both untouched for a later UI pass.
impl crate::drop_gate::QueuedDrops for WebApp {
    type Dropped = egui::DroppedFileHandle;
    type Pending = String;

    fn drops_wait(&self) -> bool {
        crate::drop_gate::drops_wait(&self.app)
    }

    fn defer_dropped(&mut self) {
        self.app.defer_dropped_files();
    }

    fn take_dropped(&mut self) -> Vec<Self::Dropped> {
        self.app.take_dropped_files()
    }

    fn take_unprotected_dropped(&mut self) -> Vec<Self::Dropped> {
        self.app.take_unprotected_dropped_files()
    }

    fn take_pending(&mut self) -> Vec<Self::Pending> {
        self.originals.take_pending()
    }
}

impl WebApp {
    /// Construct the web app from the boot options: install originals and open the library from
    /// browser storage (a read failure starts an empty session and a blocking `LibraryProblem`),
    /// load the stored UI state, start render workers and the optional benchmark.
    fn new(cc: &eframe::CreationContext<'_>, boot: Boot, chinese_font: Vec<u8>) -> Self {
        let Boot { opts, backend, files, index, lib_dir, load_error } = boot;
        let originals = Originals::default();
        let t = perf_now();
        let mut session = Session::new();
        originals.install(&mut session);
        let stores = LibraryStores {
            dir: format!("browser:{}/{lib_dir}", backend.as_ref().map_or("memory", |b| b.kind())).into(),
            catalog: Box::new(files.store()),
            files: Box::new(files.store()),
            on_disk: false,
        };
        let mut problem = None;
        // a library that couldn't be read is not opened: it would show incomplete, then save over the stored copy
        let read_failed = load_error.is_some();
        let opened = match load_error {
            Some(e) => Err(format!("{e}. Nothing was changed in storage")),
            None => session.open_library_in(stores, true).cloned().map_err(|e| e.to_string()),
        };
        match opened {
            Ok(r) => log::info!(
                "lightcraft: library {} in {:.0} ms ({} photos; snapshot seq {}, {} ops replayed)",
                if r.created { "created" } else { "loaded" },
                perf_now() - t,
                session.catalog.len(),
                r.snapshot_seq,
                r.replayed
            ),
            Err(e) => {
                // never a silent demo session (issue #100): an empty one, and the window says why
                log::error!("opening the library failed: {e}");
                session = Session::new();
                originals.install(&mut session);
                problem = Some(lightcraft_ui_egui::panels::library_problem::LibraryProblem {
                    can_retry: false,
                    ..lightcraft_ui_egui::panels::library_problem::LibraryProblem::new(
                        "the browser's storage for this page",
                        format!("{e}. Reload the page to try again."),
                    )
                });
            }
        }
        if backend.is_some() {
            notice(
                "LightCraft in the browser is experimental. Your library is kept in this browser's storage: keep your original photos \
                 elsewhere and back up with File ▸ Back Up Library….",
            );
        }
        // previews are large (≤ 2560 px, f32): keep few in a 32-bit address space
        session.media.preview_capacity = 3;
        // nothing is saved while the stored library couldn't be read
        let frozen = Rc::new(Cell::new(read_failed));
        let mut app = LightcraftApp::new(session, services(originals.clone(), backend.clone(), files.clone(), frozen.clone(), cc.egui_ctx.clone()))
            .with_chinese_font(chinese_font);
        let ui_written = files.get("ui.json").unwrap_or_default();
        // an arrangement from an older docking version starts again from the default
        let mut saved = serde_json::from_slice::<serde_json::Value>(&ui_written).unwrap_or_default();
        if let Some(fields) = saved.as_object_mut() {
            lightcraft_ui_egui::docking::drop_outdated(fields);
        }
        if let Ok(ui) = serde_json::from_value::<UiState>(saved) {
            app.ui = ui;
        }
        app.ui = app.ui.sanitized();
        app.library_problem = problem;
        let n = opts.workers.unwrap_or_else(|| {
            let cores = window().map_or(1, |w| w.navigator().hardware_concurrency() as usize);
            cores.saturating_sub(1).clamp(1, 4)
        });
        let cache = app.session.media.rendered.clone();
        let workers = (n > 0).then(|| {
            Workers::start(
                n,
                backend.as_ref().map_or("memory", |b| b.kind()),
                backend.clone(),
                index,
                &cache,
                app.session.cache_bytes(),
                cc.egui_ctx.clone(),
            )
        });
        if let Some(w) = &workers {
            app.renderer.set_offload(Box::new(w.clone()));
        }
        log::info!("lightcraft: {n} render workers");
        CTX.with(|c| *c.borrow_mut() = Some(cc.egui_ctx.clone()));
        let origin = lightcraft_ui_egui::monotonic_ms();
        WebApp {
            app,
            originals,
            files,
            backend,
            workers,
            flushing: Rc::new(Cell::new(false)),
            flush_failed_at: Rc::new(Cell::new(0.0)),
            frozen,
            lib_dir,
            bench: opts.bench.then(|| Bench::new(origin)),
            first_frame_logged: false,
            title: String::new(),
            last_save: 0.0,
            ui_written,
        }
    }

    /// 跟随宿主语言：首帧与宿主切换语言后的下一帧，把宿主 locale 写进 `ui.language`。
    /// `logic` 每帧都会 `i18n::set_language(self.ui.language)`，改这一个字段就够了。
    fn sync_host_language(&mut self) {
        let sync = !DID_INIT_LANG.with(|f| f.replace(true)) || NEEDS_LANG_SYNC.with(|f| f.replace(false));
        if sync
            && let Some(locale) = lightcraft_ui_egui::i18n::Locale::parse_tag(&crate::otools::host_language())
        {
            self.app.ui.language = locale;
        }
    }

    /// Consume the drops the raw-input hook queued once (never re-read `raw.dropped_files` in a
    /// hidden logic tick), and add originals whose bytes have arrived.
    fn import_dropped(&mut self, ctx: &egui::Context) {
        // A prompt or the blocking library window (including the chooser's own New Library name
        // prompt or asynchronous folder picker) holds both queues for a later UI pass: nothing is
        // read or imported behind it. `drain_ready` is the same orchestration the native tests run.
        let dialog_open = self.app.ui.dialog.is_some();
        let Some(drained) = crate::drop_gate::drain_ready(self, dialog_open) else { return };
        // a dialog is on screen (the card said so while the files hovered): nothing is read or imported
        // (files read from an earlier drop still finish below)
        if !drained.dropped.is_empty() && dialog_open {
            self.app.toast(ctx, lightcraft_ui_egui::i18n::tr("Close the open dialog, then drop the files again"));
            return;
        }
        for f in drained.dropped {
            let (originals, backend, ctx) = (self.originals.clone(), self.backend.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let name = egui::DroppedFile::path(f.as_ref()).to_string_lossy().to_string();
                match egui::DroppedFile::bytes_async(f.as_ref()).await {
                    Ok(bytes) => store_file(originals, backend, name, bytes, ctx).await,
                    Err(e) => notice(lightcraft_ui_egui::i18n::dropped_file_unreadable(&name, &e.to_string())),
                }
            });
        }
        let Some(paths) = drained.pending else { return };
        if !paths.is_empty() {
            match self.app.run("library.import", json!({"paths": paths})) {
                // what the import did, not what was submitted: duplicates and unsupported files add nothing,
                // a drop that only finds duplicates says nothing (as the desktop import's toast does when it
                // shows where they are), and one whose files could not be read says so
                Ok(report) => {
                    if let Some(notice) = lightcraft_ui_egui::i18n::dropped_photos_outcome(&report) {
                        self.app.toast(ctx, notice);
                    }
                }
                Err(e) => {
                    log::warn!("import: {e}");
                    self.app.toast(ctx, lightcraft_ui_egui::i18n::dropped_import_failed(&e));
                }
            }
        }
    }

    /// Load originals the main thread asked for (and the active photo's, ahead of time).
    fn load_originals(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.app.session.selection.active
            && let Some(Source::File { path }) = self.app.session.catalog.photo(id).map(|p| &p.source)
            && !self.originals.contains(path)
        {
            self.originals.prefetch(path);
        }
        let Some(b) = &self.backend else { return };
        for hash in self.originals.take_wanted() {
            let (b, originals, ctx) = (b.clone(), self.originals.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                match b.read(&storage_key(&hash)).await {
                    Ok(Some(bytes)) => {
                        originals.insert(&hash, Arc::from(bytes));
                        ctx.request_repaint();
                    }
                    Ok(None) => {
                        log::warn!("original {hash} is not in browser storage");
                        originals.load_failed(&hash);
                    }
                    Err(e) => {
                        log::warn!("loading original {hash}: {e}");
                        originals.load_failed(&hash);
                    }
                }
            });
        }
    }

    /// Save view state, UI prefs and the thumbnail index now and then; flush dirty files.
    fn save(&mut self) {
        if self.frozen.get() {
            return;
        }
        let now = lightcraft_ui_egui::monotonic_ms();
        if now - self.last_save >= SAVE_EVERY_MS {
            self.last_save = now;
            self.app.session.save_view();
            if let Ok(ui) = serde_json::to_vec_pretty(&self.app.ui)
                && ui != self.ui_written
            {
                self.files.write("ui.json", &ui);
                self.ui_written = ui;
            }
            if let (Some(w), Some(b)) = (&self.workers, &self.backend)
                && let Some(index) = w.take_index_if_dirty()
            {
                let b = b.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(e) = b.write(THUMB_INDEX, &index).await {
                        log::warn!("saving the thumbnail index: {e}");
                    }
                });
            }
        }
        // after a failure, retry every couple of seconds rather than every frame
        let backing_off = self.files.error().is_some() && now - self.flush_failed_at.get() < SAVE_RETRY_MS;
        if let Some(b) = &self.backend
            && self.files.is_dirty()
            && !self.flushing.get()
            && !backing_off
        {
            self.flushing.set(true);
            let (files, dir, flushing, failed_at) = (self.files.clone(), self.lib_dir.clone(), self.flushing.clone(), self.flush_failed_at.clone());
            wasm_bindgen_futures::spawn_local(flush(files, b.clone(), dir, flushing, failed_at));
        }
    }
}

type Reply = (js_sys::Function, js_sys::Function);

thread_local! {
    /// Commands from JavaScript ([`command`]), run on the next frame.
    static INBOX: std::cell::RefCell<Vec<(String, String, Reply)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run a command by id from JavaScript (automation, tests): `await command("library.info", "{}")`
/// resolves to the result as JSON text, or rejects with the error. Besides every engine/UI
/// command, `web.stats` reports the host's state (storage, workers, originals in memory).
/// OTools 里入口 HTML 可以经 `window.__OTLIGHTCRAFT_COMMAND__` 之类把命令排进来；
/// 上游网页版经 wasm 胶水的同名导出使用。
#[wasm_bindgen]
pub fn command(id: String, params: String) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, reject| {
        INBOX.with(|q| q.borrow_mut().push((id.clone(), params.clone(), (resolve, reject))));
        request_repaint();
    })
}

impl WebApp {
    fn run_inbox(&mut self) {
        let inbox = INBOX.with(|q| std::mem::take(&mut *q.borrow_mut()));
        for (id, params, (resolve, reject)) in inbox {
            let params: serde_json::Value = serde_json::from_str(&params).unwrap_or(json!({}));
            let r = if id == "web.stats" { Ok(self.web_stats()) } else { self.app.run(&id, params) };
            let _ = match r {
                Ok(v) => resolve.call1(&JsValue::NULL, &v.to_string().into()),
                Err(e) => reject.call1(&JsValue::NULL, &e.into()),
            };
        }
    }

    fn web_stats(&self) -> serde_json::Value {
        let (alive, ready, remote, inline) = self.workers.as_ref().map_or((0, 0, 0, 0), Workers::stats);
        let (orig_n, orig_bytes) = self.originals.usage();
        json!({
            "storage": self.backend.as_ref().map_or("memory", |b| b.kind()),
            "library": self.lib_dir,
            "dirty": self.files.is_dirty(),
            "flushing": self.flushing.get(),
            "saveError": self.files.error(),
            "workers": {"alive": alive, "ready": ready, "remoteJobs": remote, "inlineJobs": inline},
            "originalsInMemory": orig_n,
            "originalBytesInMemory": orig_bytes,
            "renderQueue": self.app.renderer.queued(),
            "rendersInFlight": self.app.renderer.in_flight(),
            "thumbTextures": self.app.renderer.thumb_textures(),
        })
    }
}

impl eframe::App for WebApp {
    /// eframe's non-drawing pass for the browser host: drain safety notices and queued inbox work,
    /// load originals the UI requested, apply the cache budget, run the app's logic, then save.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for text in safety::take_notices() {
            let now = self.app.tick_now();
            self.app.ui.toast = Some((text, now + NOTICE_SECS, None));
        }
        self.sync_host_language();
        // 宿主投递的「打开方式」：系统里双击已绑定扩展名的照片 → 存进浏览器存储并
        // 排队导入，与「从设备添加」、拖放走同一条路（`import_dropped` 会取走它们）。
        for (name, _path, bytes) in crate::otools::drain_delivered() {
            let (originals, backend, ctx) = (self.originals.clone(), self.backend.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                store_file(originals, backend, name, bytes, ctx).await;
            });
        }
        self.run_inbox();
        self.load_originals(ctx);
        if let Some(w) = &self.workers {
            // `library.preferences ▸ cacheMb` is read by the session; the worker index prunes on
            // its own thread, so it gets the budget every frame and applies it on the next store.
            w.set_budget(self.app.session.cache_bytes());
        }
        self.app.logic(ctx);
        self.save();
    }

    /// Pass the browser's raw input to the app hook before egui processes it, so prompt barriers
    /// and synthetic or dropped input are applied consistently with the desktop host.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw);
    }

    /// eframe's UI pass: import queued drops unless a prompt blocks input, draw the app, update the
    /// page title, and advance the benchmark once per root pass.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drops are the UI phase's: a hidden `logic`/`run_logic` tick must not import them twice.
        // `import_dropped` itself takes both queues only through the gate, so a prompt or the
        // blocking library window (over an already open library) keeps a stored drop queued.
        let ctx = ui.ctx().clone();
        self.import_dropped(&ctx);
        self.app.ui(ui);
        // eframe's web runner ignores `ViewportCommand::Title`: name the open photo in the page title
        let title = self.app.page_title(self.files.is_dirty() || self.flushing.get());
        if title != self.title
            && let Some(doc) = window().and_then(|w| w.document())
        {
            doc.set_title(&title);
            self.title = title;
        }
        // The benchmark is a UI-phase step: a second layout pass or a hidden tick must not count a frame.
        if ui.ctx().current_pass_index() == 0 {
            if let Some(b) = self.bench.as_mut()
                && let Some(report) = b.step(&mut self.app)
            {
                log::info!("lightcraft-bench {report}");
                if let Some((alive, ready, remote, inline)) = self.workers.as_ref().map(Workers::stats) {
                    log::info!("lightcraft-workers {{\"alive\":{alive},\"ready\":{ready},\"remote_jobs\":{remote},\"inline_jobs\":{inline}}}");
                }
                set_status(&format!("bench: {report}"));
                self.bench = None;
            }
            if self.bench.is_some() {
                ctx.request_repaint();
            }
        }
        if !self.first_frame_logged && !self.app.widgets.is_empty() {
            self.first_frame_logged = true;
            log::info!("lightcraft first UI frame at {:.0} ms after navigation start", perf_now());
        }
    }
}

/// "Auto" follows the browser's languages (issue #2085): the first of `navigator.languages` (else
/// `navigator.language`) that LightCraft ships becomes the UI's default. Every step is fallible
/// (as in [`crate::safety::acquire_tab_lock`]): without a usable navigator Auto stays English,
/// as before.
fn install_browser_language() {
    let Ok(navigator) = js_sys::Reflect::get(&js_sys::global(), &"navigator".into()) else { return };
    let mut tags: Vec<String> = Vec::new();
    if let Ok(languages) = js_sys::Reflect::get(&navigator, &"languages".into())
        && let Some(languages) = languages.dyn_ref::<js_sys::Array>()
    {
        for tag in languages.iter() {
            if let Some(tag) = tag.as_string() {
                tags.push(tag);
            }
        }
    }
    if tags.is_empty()
        && let Ok(language) = js_sys::Reflect::get(&navigator, &"language".into())
        && let Some(language) = language.as_string()
    {
        tags.push(language);
    }
    if let Some(language) = lightcraft_ui_egui::i18n::locale_from_tags(&tags) {
        lightcraft_ui_egui::i18n::set_default_language(language);
        log::info!("lightcraft: default language {language:?} from the browser's languages");
    }
}

/// Start the app with the separately shipped Simplified Chinese font (empty when it couldn't
/// be fetched: 中文界面会显示方框，但不影响启动).
pub fn start() {
    // trunk 把 `main` 自动标成 wasm-bindgen start：渲染 worker 里实例化同一个模块也会跑到
    // 这里。worker 没有 window、更不该起应用——直接让路，worker.js 自会走 `worker_main`。
    if web_sys::window().is_none() {
        return;
    }
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    safety::install_panic_hook();
    install_browser_language();
    log::info!("lightcraft: wasm instantiated at {:.0} ms", perf_now());
    log::info!("otools host bridge available: {}", crate::otools::available());
    wasm_bindgen_futures::spawn_local(async move {
        // 中文字体是独立文件（嵌进 wasm 会超宿主的体积上限，上游网页版同款做法）。
        let chinese_font = crate::otools::fetch_bytes("./fonts/otools-zh-hans.otf").await.unwrap_or_else(|e| {
            log::warn!("lightcraft: no Simplified Chinese font ({e}); Chinese-only glyphs will show as boxes");
            Vec::new()
        });
        // one tab per library: two would each keep their own copy and overwrite each other's saves
        let opts = Options::from_url();
        let memory = opts.store == "memory";
        if !memory && safety::acquire_tab_lock().await == Some(false) {
            safety::show_blocking(
                "LightCraft is already open in another tab or window of this browser.\n\n\
                 Switch to that tab, or close it and reload this page. (Two tabs would overwrite each other's changes.)",
            );
            return;
        }
        let Some(canvas) = window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("lightcraft_canvas"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("missing <canvas id=\"lightcraft_canvas\">");
            return;
        };
        let boot = boot(opts).await;
        let runner = eframe::WebRunner::new();
        let r = runner
            .start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(WebApp::new(cc, boot, chinese_font)))))
            .await;
        match r {
            Ok(()) => {
                if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_loading")) {
                    el.remove();
                }
            }
            Err(e) => {
                log::error!("LightCraft failed to start: {e:?}");
                set_status(&format!("LightCraft failed to start: {e:?}"));
            }
        }
        install_locale_listener();
    });
}

/// 监听宿主语言切换（OTools 注入的 `otools-locale-changed` 事件）：置位标志并请求重绘，
/// 下一帧 `logic` 里的 `sync_host_language` 会把新 locale 写进 `ui.language`。
fn install_locale_listener() {
    let Some(window) = web_sys::window() else { return };
    let closure = Closure::wrap(Box::new(|| {
        NEEDS_LANG_SYNC.with(|f| f.set(true));
        request_repaint();
    }) as Box<dyn FnMut()>);
    let _ = window.add_event_listener_with_callback(
        "otools-locale-changed",
        closure.as_ref().unchecked_ref::<js_sys::Function>(),
    );
    // 宿主投递了文件：同一份「置位 + 请求重绘」逻辑，让下一帧的 logic 去取件。
    let _ = window.add_event_listener_with_callback(
        "otools-open-files-arrived",
        closure.as_ref().unchecked_ref::<js_sys::Function>(),
    );
    closure.forget();
}
