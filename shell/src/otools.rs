//! OTools 宿主桥（`window.lightcraftOtools` 的 wasm-bindgen 绑定）+ 浏览器 fetch 工具。
//!
//! JS 侧的实现见 `shell/index.html`。宿主能力有两条路（宿主包装 API 与 Tauri 原生 IPC），
//! JS 里已经统一处理，这里只关心「可用性 / 语言 / 打开文件 / 保存文件」四个函数，
//! 外加一个启动时取中文字体用的 fetch。

use base64::Engine as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::js_sys;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = lightcraftOtools, js_name = "available")]
    fn host_available() -> bool;

    /// 宿主界面语言（BCP 47，如 `zh-CN`）：OTools 注入到插件 webview 的
    /// `window.__OTOOLS_LOCALE__.locale` / `document.documentElement.lang`。
    #[wasm_bindgen(js_namespace = lightcraftOtools, js_name = "language")]
    fn host_language_raw() -> String;

    /// `{title, extensions}` → `[{name, path, dataBase64}]`。
    #[wasm_bindgen(js_namespace = lightcraftOtools, js_name = "openFiles")]
    fn host_open_files(request: JsValue) -> js_sys::Promise;

    /// `{name, dataBase64}` → `{path}` 或 `null`（用户取消）。
    #[wasm_bindgen(js_namespace = lightcraftOtools, js_name = "saveFile")]
    fn host_save_file(request: JsValue) -> js_sys::Promise;
}

/// 一次打开的结果：`(文件名, 宿主的真实路径, 字节)`。
pub type OpenedFile = (String, String, Vec<u8>);

pub fn available() -> bool {
    host_available()
}

pub fn host_language() -> String {
    host_language_raw()
}

/// 社区/外部链接（Discord、ArtCraft 官网、上游 GitHub）：在 OTools 插件里点这些
/// 会把用户拽到外部，让插件不纯粹。`open_url` 钩子对命中者返回 `Ok(())` 静默吞掉。
pub fn is_community_link(url: &str) -> bool {
    url.contains("discord.gg") || url.contains("getartcraft.com") || url.contains("github.com/storytold")
}

pub fn open_request(title: &str, exts: &[&str]) -> JsValue {
    let request = js_sys::Object::new();
    let arr = js_sys::Array::new();
    for e in exts {
        arr.push(&JsValue::from_str(e));
    }
    let _ = js_sys::Reflect::set(&request, &JsValue::from_str("title"), &JsValue::from_str(title));
    let _ = js_sys::Reflect::set(&request, &JsValue::from_str("extensions"), &arr);
    request.into()
}

/// 弹宿主的打开对话框并读出选中的文件。
pub async fn open_files(request: &JsValue) -> Result<Vec<OpenedFile>, String> {
    let value = JsFuture::from(host_open_files(request.clone())).await.map_err(js_error)?;
    let mut out = Vec::new();
    for entry in js_sys::Array::from(&value).iter() {
        let name = field(&entry, "name").unwrap_or_else(|| "photo.jpg".to_string());
        let path = field(&entry, "path").unwrap_or_else(|| name.clone());
        let data = field(&entry, "dataBase64").unwrap_or_default();
        let bytes = decode(&data)?;
        out.push((name, path, bytes));
    }
    Ok(out)
}

/// 把字节交给宿主写盘（导出、备份库等）。宿主写盘是异步的：这里发射后不管，
/// 结果只进日志——调用方（`Services.write`）按「已受理」处理。
pub fn save_file(name: &str, bytes: &[u8]) {
    let data = encode(bytes);
    let name = name.to_string();
    wasm_bindgen_futures::spawn_local(async move {
        let request = {
            let request = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&request, &JsValue::from_str("name"), &JsValue::from_str(&name));
            let _ = js_sys::Reflect::set(&request, &JsValue::from_str("dataBase64"), &JsValue::from_str(&data));
            request.into()
        };
        match JsFuture::from(host_save_file(request)).await {
            Ok(value) if value.is_null() || value.is_undefined() => log::warn!("保存被取消：{name}"),
            Ok(value) => match field(&value, "path") {
                Some(path) => log::info!("已保存 {name} -> {path}"),
                None => log::info!("已保存 {name}"),
            },
            Err(e) => log::error!("保存失败：{name}：{}", js_error(e)),
        }
    });
}

/// `fetch(url)` 的字节数组：启动时取 `dist/fonts/` 里的中文字体。
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let global = js_sys::global();
    let fetch = js_sys::Reflect::get(&global, &JsValue::from_str("fetch"))
        .map_err(|_| "window.fetch 不可用".to_string())?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| "window.fetch 不是函数".to_string())?;
    let response = fetch
        .call1(&global, &JsValue::from_str(url))
        .map_err(|e| js_error(e))?
        .dyn_into::<js_sys::Promise>()
        .map_err(|_| "fetch 没有返回 Promise".to_string())?;
    let response = JsFuture::from(response).await.map_err(js_error)?;
    let ok = js_sys::Reflect::get(&response, &JsValue::from_str("ok"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !ok {
        return Err(format!("{url}：HTTP 状态异常"));
    }
    let buffer = js_sys::Reflect::get(&response, &JsValue::from_str("arrayBuffer"))
        .map_err(|_| "response.arrayBuffer 不可用".to_string())?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| "response.arrayBuffer 不是函数".to_string())?
        .call0(&response)
        .map_err(js_error)?;
    let buffer = JsFuture::from(buffer.dyn_into::<js_sys::Promise>().map_err(|_| "arrayBuffer 没有返回 Promise")?)
        .await
        .map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode(data: &str) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Err("宿主返回的文件内容为空".into());
    }
    base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("宿主返回的内容不是合法的 base64：{e}"))
}

fn field(value: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(value, &JsValue::from_str(key)).ok()?.as_string()
}

fn js_error(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}
