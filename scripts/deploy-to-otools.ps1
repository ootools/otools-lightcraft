# 把插件安装进 OTools 的本地插件目录（本地试跑用，**不碰 otools 仓库**）
# 用法：pwsh -File scripts/deploy-to-otools.ps1 [-PluginDir "$env:USERPROFILE\.otools\plugins\otools-lightcraft"]
#
# 目标目录就是宿主加载插件的地方（`otools_root_dir()/plugins/<packid>`，见 otools 仓库
# crates/ot-storage/src/lib.rs 的 OTOOLS_DIR = ".otools"）。放在仓库外，所以本地试跑
# 不会往 otools 仓库里丢任何文件（宿主仓 .gitignore 只忽略 dist，写进仓库反而会留下
# plugin.json/logo.svg 这类未跟踪杂物）。
#
# 只安装「宿主真正需要的」：plugin.json / logo / 入口与构建产物 dist/。
# upstream/、shell/、scripts/、docs 都留在本仓库，不进安装目录。
param(
  [string]$PluginDir = (Join-Path $env:USERPROFILE ".otools\plugins\otools-lightcraft")
)

$ErrorActionPreference = "Stop"
$pluginRoot = Split-Path -Parent $PSScriptRoot

if (-not (Test-Path (Join-Path $pluginRoot "dist\index.html"))) {
  Write-Error "还没有构建产物 dist\index.html，先跑 node scripts/build-wasm.mjs"
}

New-Item -ItemType Directory -Force -Path $PluginDir | Out-Null

# 需要随插件分发的东西：清单、图标、入口与构建产物
$ship = @(
  "plugin.json",
  "logo.svg",
  "dist"
)

foreach ($item in $ship) {
  $from = Join-Path $pluginRoot $item
  if (-not (Test-Path $from)) { continue }
  if (Test-Path (Join-Path $PluginDir $item)) {
    Remove-Item -LiteralPath (Join-Path $PluginDir $item) -Recurse -Force
  }
  Copy-Item -LiteralPath $from -Destination $PluginDir -Recurse -Force
}

Write-Host "已安装到 $PluginDir"
Write-Host "在 otools 里重载/重启应用即可看到「LightCraft」插件。"
