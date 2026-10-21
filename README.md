# LightCraft · OTools 插件

把 [storytold/lightcraft](https://github.com/storytold/lightcraft)（开源、纯 Rust 重写的
Adobe Lightroom 式照片库与 RAW 冲图，ArtCraft 家族的一员）移植为 OTools 插件：
上游的 egui UI 原封不动编译到 WebAssembly，图库/目录/缩略图/偏好存浏览器存储
（与上游网页版一致），「照片从哪来、导出到哪去」换成 OTools 宿主的对话框与磁盘读写。

- 插件目录：`plugins/otools-lightcraft`（在 [ootools/OTools](https://github.com/ootools/OTools) 主仓里）
- 上游源码：**不入库**，见 `upstream.pin` + `scripts/fetch-upstream.mjs`
- 中文字体：**不入库**，见 `fonts.pin` + `scripts/fetch-fonts.mjs`（Noto Sans CJK SC，OFL）

## 功能与折扣

保留：图库（相册/文件夹/评分/旗标/色标/搜索）、RAW 冲图（曝光/曲线/色彩分级/蒙版/去雾等）、
裁剪、预设、HDR/全景合成、导出、备份/恢复库、渲染 worker（后台出图）、14 种界面语言。

折扣（与上游网页版相同，见 AGENTS.md）：

- 预设/关键词/曲线等小文件的打开对话框未接宿主（上游网页版也未接）；
- 显示器 ICC 配置、外部编辑器、在文件管理器中显示是桌面专属，不可用；
- WebGPU 渲染仍在上游进行中，出图走上游现有的管线。

## MCP / AI 自动化

插件同时提供一个可被 OTools 宿主 MCP 网关加载的 native 适配层：`native/` 把上游
`lightcraft-mcp` 的 headless 后端编成动态库，宿主用一个统一 MCP 端点暴露工具。
当前完整保留上游 **388** 个工具；`plugin.json` 的 `mcp.expose` 只是设置页的默认勾选种子，
其余工具可由用户显式授权。会话跨调用持久（例如 `command_run → doc_inspect`）。

```bash
cargo test --release --manifest-path native/Cargo.toml
cargo build --release --manifest-path native/Cargo.toml
```

正式包包含 `lib/{macOS.dylib,Windows.dll,Linux.so}`；macOS 为 arm64 + x86_64 的
universal dylib。native 产物不进 git，由 `release-plugin.yml` 的跨平台 job 构建。

## 上游版本

打包所用的上游提交。`upstream.pin` 是唯一真源，这张表用来回溯「哪个插件版本打的是哪份上游代码」，
每次发布都要在这里加一行。

| 插件版本 | 上游标签 | 上游提交 | 打包日期 |
| --- | --- | --- | --- |
| 0.1.2 | v0.9.1 | `4f0b110bcbd2e350c6d97b47f1d824c2222295c4` | 2026-10-21 |
| 0.1.1 | — | `0da998f24dfc03069d49867f8877af2bf7c51f94` | — |

## 构建

```bash
unset RUSTC_WRAPPER                 # 它会把 cargo 指向旧 rustc
node scripts/build-wasm.mjs         # 拉上游+字体 → trunk build --release → dist/
```

需要 rustc >= 1.95 与 wasm32-unknown-unknown 目标（egui 0.36 的要求）。

```bash
node scripts/serve-dist.mjs         # 本地起 http://127.0.0.1:6488 试 dist/
pwsh -File scripts/deploy-to-otools.ps1   # 装进 OTools 本地插件目录（~/.otools/plugins，不碰任何仓库）
node scripts/pack-plugin.mjs        # 打 .oplg（配合 verify-plugin.mjs 校验）
```

详细约定见 [AGENTS.md](./AGENTS.md)。
