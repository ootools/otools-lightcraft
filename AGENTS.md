# AGENTS.md —— otools-lightcraft 移植约定

给后续接手这个仓库的 agent（和人）的规则。核心一句话：**`upstream/` 与
`shell/src/` 里的上游副本一行都不要改**，所有移植代码集中在 `web.rs` /
`otools.rs` / `main.rs`，宿主 MCP 适配层单独放在 `native/`，这样上游同步永远干净。

## 铁律

1. **`upstream/` 保持上游原样**。它是 `storytold/lightcraft` 在 `upstream.pin`
   里那个 commit 的浅拉取结果，**不在 git 里**（见 `scripts/fetch-upstream.mjs`）。
   任何「顺手改一下上游」的念头都会在下次同步时丢失或变成冲突。
2. **`shell/src/{backend,backup,bench,files,safety,store,wire,workers}.rs` 是上游
   `apps/lightcraft-web/src/` 的逐字副本**（连文件名都一致，方便
   `diff upstream/apps/lightcraft-web/src shell/src` 同步）。要改行为去改
   `shell/src/web.rs`（上游 web.rs 的改写版）或 `shell/src/otools.rs`。
3. **`shell/` 不引入 native cdylib**。OTools 宿主已经注入了
   `tools_webview_read_file` / `tools_webview_write_file` 与文件对话框；库与原图的
   持久化走浏览器存储（OPFS/IndexedDB），和上游网页版同款，别往宿主盘上写库。
   **例外（已确立）**：`native/` 是为宿主 MCP 网关暴露 headless 能力的独立适配层，
   不参与 WASM 构建，也不是前端外壳的替代。
4. **改 `shell/Cargo.toml` 的上游依赖版本时，对齐 `upstream/Cargo.toml` 的
   `[workspace.dependencies]`**（egui/eframe 目前是 `0.36` 这一档）。

## 上手前必须知道的三件事

- **rustc 版本**：上游 `rust-version = "1.95"`（egui 0.36 用了 1.95 才稳定的
  `Vec::push_mut`）。本机 1.94 会在 `epaint` 上直接 `E0658`。
  `scripts/build-wasm.mjs` 会检查并提示。本机环境变量 `RUSTC_WRAPPER=sccache`
  指向 PATH 里的旧 rustc，会让 cargo 用错工具链——构建前 `unset RUSTC_WRAPPER`。
- **git 拉取优先走 SSH**：HTTPS 直连 github.com 在本机会偶发超时（21s 连接失败），
  `fetch-upstream.mjs` 的策略是「SSH 优先，回落 HTTPS」；字体走 raw.githubusercontent.com。
- **trunk 首次运行会下载 wasm-bindgen-cli 与 wasm-opt**（GitHub release），
  国内网络可能要直连；CI 里已配 `CARGO_NET_GIT_FETCH_WITH_CLI`。

## 上游管理：pin + 按需拉取（不是 subtree / submodule）

上游源码**不在**本仓库里，只有 `upstream.pin` 记录了仓库与精确的 40 位 commit：

```json
{ "upstream": "storytold/lightcraft", "ssh": "…", "https": "…", "ref": "main", "commit": "…" }
```

- **本地/CI 取源码**：`node scripts/fetch-upstream.mjs`（缺失才拉，已对齐就跳过；
  `--force` 丢弃本地改动重拉）。GitHub 允许按任意可达 commit 做 `--depth 1` 拉取。
- **同步上游更新**：改 `upstream.pin` 的 `commit` 为新的完整 SHA，重新拉取，然后
  `diff upstream/apps/lightcraft-web/src shell/src` —— 除 `web.rs` 外应当零差异；
  上游 web.rs 有变化时把它再「移植」进我们的 `shell/src/web.rs`（只保留四处差异，
  见下）。`fonts.pin` 同理管字体（只按 SHA+sha256 取单个文件，不整库克隆 68 MB）。

## 移植层的地图

`shell/src/web.rs` 相对上游 `apps/lightcraft-web/src/web.rs` 只有四处差异：

| 差异 | 位置 | 说明 |
| --- | --- | --- |
| 导入走宿主对话框 | `pick_via_host` + `services.pick_files` | 字节仍走上游的 `store_file`（内容寻址存进浏览器存储） |
| 导出交给宿主写盘 | `services.write` | 替换上游的浏览器下载；发射后不管，取消/失败只进日志 |
| 暗黑主题钉死 | `services.system_theme` | 恒报 Dark；Appearance=Auto 也不跟随宿主浅色 |
| 语言跟随宿主 | `sync_host_language` + `install_locale_listener` | 首帧与 `otools-locale-changed` 后把宿主 locale 写进 `ui.language` |

另有删减：上游的 `<input type=file>` 选择器与「下载」导出已删（宿主替代）；
`command` 控制通道保留（wasm-bindgen 导出，自动化可用）。

| 文件 | 作用 |
| --- | --- |
| `shell/index.html` | 唯一的 JS：`window.lightcraftOtools`（available/language/openFiles/saveFile） |
| `shell/src/otools.rs` | 桥：`open_files` / `save_file` / `fetch_bytes` / 语言 |
| `shell/src/web.rs` | 上游 web.rs 的改写版（上表四处差异） |
| `shell/worker.js` | 渲染 worker 模板；构建后由 build-wasm.mjs 把胶水文件名替换成 trunk 实际产物 |
| `fonts.pin` + `scripts/fetch-fonts.mjs` | Noto Sans CJK SC Regular（OFL），按 pinned commit + sha256 取 |
| `upstream/` | （不在 git 里）上游源码，都是 path 依赖直接复用 |
| `native/` | 宿主 MCP 网关的 headless 适配层（独立 workspace，见下节） |

## 持久化模型（为什么库不落宿主盘）

目录、原图、缩略图、UI 偏好全在浏览器存储（OPFS 优先，不行退 IndexedDB），
和上游网页版完全一致：

- 好处：上游的 `files` / `store` / `backend` / `workers` / `backup` 模块逐字复用，
  增量保存、缩略图索引、备份/恢复库全部免费拿到；
- 代价：webview 的存储可能被系统清（OPFS 持久化已在启动时申请），
  所以「File ▸ Back Up Library…」在插件里同样可用且值得用；
- 别把库搬到宿主磁盘：那意味着重写五个上游模块，破坏同步性，收益却只有存储位置。

## native 适配层（宿主 MCP 网关）

`native/` 把上游现成的 `crates/mcp` headless 后端编成动态库，让外部 AI 工具经宿主的
单一 MCP 端点操作 LightCraft。它不参与 WASM 构建，与 `shell/` 互不依赖。

- **独立 workspace**：并进上游 workspace 会把 GUI/GPU 依赖也拉进插件构建。
- **上游源码不在 git 里**：构建前先 `node scripts/fetch-upstream.mjs`。
- **专用线程持有常驻 `Server`**：上游 `Server` 持有 `Box<dyn Backend>` 且不保证
  `Send`。`native/src/mcp.rs` 在线程内保留唯一 Server，宿主调用经 channel 串行进入，
  以复用上游工具表、参数校验、注解与错误格式；`session_state_persists_across_calls`
  用例守着跨调用会话状态。
- **完整工具面**：上游默认生成每个命令一个 `cmd_*` 工具（本版本 388 个）。适配层
  不裁剪，manifest 的 `mcp.expose` 只提供默认白名单种子，其余可在设置页授权。
- **宿主 method**：`__otools_mcp_tools__` / `__otools_mcp_call__` /
  `__otools_shutdown__`。
- **产物**：`lib/{macOS.dylib,Windows.dll,Linux.so}`，不进 git；多平台由
  `.github/workflows/release-plugin.yml` 构建并 lipo。

## 常见任务

- **构建 dist/** → `node scripts/build-wasm.mjs`（自动 take upstream + fonts）。
- **本地试跑** → `node scripts/serve-dist.mjs`（浏览器试）或
  `scripts/deploy-to-otools.ps1` 装进 OTools 本地插件目录
  （`~/.otools/plugins/otools-lightcraft`，仓库外，不会往 otools 仓库丢文件）。
- **改 OTools 侧的元信息**（名称/图标/关键词）→ `plugin.json`。

## 不要做的事

- 不要把 `upstream/` 提交进 git（它是构建中间件，不是源码）。
- 不要给 `upstream/` 里的 crate 打 patch、加 `cfg`、写 TODO。
- 不要改 `shell/src/` 里的上游副本文件（见铁律 2）。
- 不要在本仓库跑 `cargo build` 而不管 target：外壳是 wasm-only 的，
  宿主（x86_64）下 `main()` 只会打印一行提示，别把它当编译报错。

## 状态：外壳已移植（2026-10-16）

`shell/` 复用上游整个浏览器宿主（含渲染 worker 与备份/恢复），只换宿主边界。
未接的宿主对话框（预设/关键词/曲线等小文件、去噪模型、Lightroom 目录导入）与上游
网页版一致，都留 `None`；要接就在 `web.rs` 的 `services()` 里补 `pick_*` 闭包
（异步宿主对话框 + `originals`/`files` 的路径与上游语义对齐）。
