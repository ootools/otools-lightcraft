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

## 构建

```bash
unset RUSTC_WRAPPER                 # 它会把 cargo 指向旧 rustc
node scripts/build-wasm.mjs         # 拉上游+字体 → trunk build --release → dist/
```

需要 rustc >= 1.95 与 wasm32-unknown-unknown 目标（egui 0.36 的要求）。

```bash
node scripts/serve-dist.mjs         # 本地起 http://127.0.0.1:6488 试 dist/
pwsh -File scripts/deploy-to-otools.ps1   # 同步到 otools 仓库的 plugins/otools-lightcraft
node scripts/pack-plugin.mjs        # 打 .oplg（配合 verify-plugin.mjs 校验）
```

详细约定见 [AGENTS.md](./AGENTS.md)。
