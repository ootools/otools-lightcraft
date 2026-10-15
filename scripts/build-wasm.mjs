/**
 * 构建插件前端（WASM + trunk → dist/）。
 * ------------------------------------------------------------------
 * 上游要求 rustc >= 1.95（egui 0.36 用了 1.95 才稳定的 Vec::push_mut）；
 * 本机 1.94 会在 epaint 上直接 E0658，脚本会拦住并提示升级。
 *
 * 用法：node scripts/build-wasm.mjs [--debug]
 *
 * 依赖：
 *   - trunk           （cargo install trunk）
 *   - rustc >= 1.95 + wasm32-unknown-unknown 目标
 *
 * 本脚本会先按 upstream.pin 拉取上游源码（缺失才拉）、按 fonts.pin 取中文字体，
 * 再跑 trunk；最后把字体拷进 dist/fonts/，并把 worker.js 里的 wasm 胶水文件名
 * 换成 trunk 实际产物（trunk 开 filehash 后文件名带内容哈希，上游 worker.js 是
 * 写死 `lightcraft_web.js` 的，不替换渲染 worker 会起不来）。
 * trunk 首次运行会自行下载 wasm-bindgen-cli 与 wasm-opt（binaryen，走 GitHub
 * release），国内网络可能需要直连环境跑一次。
 */
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const debug = process.argv.includes('--debug');
const plugin = JSON.parse(fs.readFileSync(path.join(repoRoot, 'plugin.json'), 'utf8'));
const label = plugin.displayName ?? plugin.packid ?? 'plugin';

// stdin 必须是 'ignore'：某些机器（安全软件挂钩匿名管道）上给子进程接 stdin 管道
// 会让 CreateProcess 直接 EBUSY。
const run = (command, args, options = {}) => {
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.error) {
    console.error(`执行失败：${command} ${args.join(' ')}`);
    console.error(result.error.message);
    process.exit(1);
  }
  return result.status ?? 1;
};

const capture = (command, args, options = {}) => {
  const result = spawnSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...options });
  if (result.error) return null;
  return result.stdout?.trim() ?? '';
};

// ---------------------------------------------------------------- 上游源码

if (run('node', [path.join(repoRoot, 'scripts', 'fetch-upstream.mjs')]) !== 0) {
  console.error('上游源码就位失败：见上面的错误。');
  process.exit(1);
}

// ---------------------------------------------------------------- 中文字体
//
// 只是把字体放进 dist/ 的运行时资源（wasm 启动后 fetch），下载失败只警告不拦构建：
// 没有它中文/日文界面显示方框，但应用照常跑（上游网页版同款行为）。
if (run('node', [path.join(repoRoot, 'scripts', 'fetch-fonts.mjs')]) !== 0) {
  console.warn('! 中文字体没有就位（见上）：中日韩文字会显示方框。');
}

// ---------------------------------------------------------------- git 通道
//
// 上游 crate 里有 git 依赖（craft-ui / craft-libs / lightcraft 等）。这些 URL 写成 https，
// 而某些网络环境下访问 github.com 的 https 会 502（代理把 CONNECT 拦了），SSH 却是通的。
// 这时候用会话级的 insteadOf 把 https 改写到 ssh，只在本次构建的进程里生效，
// 不写用户配置、也不影响 CI（CI 里 https 正常，探测就不会开改写）。
function githubHttpsWorks() {
  const probe = spawnSync('git', ['ls-remote', 'https://github.com/octocat/Hello-World.git', 'HEAD'], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 20000,
  });
  return probe.status === 0;
}

const env = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: 'true' };
if (process.env.CI !== 'true' && env.OT_GIT_SSH_REWRITE !== '0' && !githubHttpsWorks()) {
  console.log('▸ github.com 的 https 不通，本次构建把 https://github.com/ 改写到 git@github.com:（仅进程内生效）');
  env.GIT_CONFIG_COUNT = '1';
  env.GIT_CONFIG_KEY_0 = 'url.git@github.com:.insteadOf';
  env.GIT_CONFIG_VALUE_0 = 'https://github.com/';
}
delete env.OT_GIT_SSH_REWRITE;

// sccache 会把 cargo 指向 PATH 里的旧 rustc，导致特性/版本不对——直接忽略它。
if (env.RUSTC_WRAPPER) {
  console.log(`▸ 忽略 RUSTC_WRAPPER=${env.RUSTC_WRAPPER}（它会让 cargo 用错工具链）`);
  env.RUSTC_WRAPPER = '';
}

// ---------------------------------------------------------------- 工具链检查

/** 找一个可用的 rustc：优先 rustup 的 default，其次 PATH 里的。 */
function resolveRustc() {
  const rustup = capture('rustup', ['which', 'rustc'], { env });
  if (rustup && fs.existsSync(rustup)) return rustup;
  const which = process.platform === 'win32' ? 'where' : 'which';
  const found = capture(which, ['rustc'], { env });
  const first = found?.split(/\r?\n/)[0];
  return first && fs.existsSync(first) ? first : null;
}

const rustc = resolveRustc();
if (!rustc) {
  console.error('找不到 rustc。请先安装 Rust（>= 1.95）：https://rustup.rs');
  process.exit(1);
}

const version = capture(rustc, ['--version'], { env }) ?? '';
const match = /(\d+)\.(\d+)\.(\d+)/.exec(version);
const [major, minor] = match ? [Number(match[1]), Number(match[2])] : [0, 0];
if (major === 0 || major < 1 || (major === 1 && minor < 95)) {
  console.error(`rustc 版本过低：${version}`);
  console.error(`${label} 需要 rustc >= 1.95。升级：rustup update stable && rustup target add wasm32-unknown-unknown`);
  process.exit(1);
}

const targets = capture('rustup', ['target', 'list', '--installed'], { env }) ?? '';
if (targets && !targets.includes('wasm32-unknown-unknown')) {
  console.error('缺少 wasm32-unknown-unknown 目标。安装：rustup target add wasm32-unknown-unknown');
  process.exit(1);
}

const trunk = capture(process.platform === 'win32' ? 'where' : 'which', ['trunk'], { env });
if (!trunk) {
  console.error('找不到 trunk。安装：cargo install trunk（或下载 https://github.com/trunk-rs/trunk/releases 的预编译二进制放入 PATH）');
  process.exit(1);
}

console.log(`rustc: ${version}`);
console.log(`trunk: ${trunk}`);
// 本机 PATH 里可能排在前面的是旧版独立安装（如 "Rust stable MSVC 1.94"），cargo 会拿它
// 当 rustc 用，epaint 直接 E0658。这里把 RUSTC 显式指到 rustup 解析出的工具链，
// trunk → cargo → rustc 就都走 1.95+（CI 上等价于 stable，无害）。
env.RUSTC = rustc;
console.log(`构建 ${label}（${debug ? 'debug' : 'release'}）→ ${path.join(repoRoot, 'dist')}`);

const status = run(trunk, ['build', debug ? '' : '--release'].filter(Boolean), {
  cwd: path.join(repoRoot, 'shell'),
  env,
});
if (status !== 0) {
  console.error('trunk 构建失败。');
  process.exit(status);
}

// ---------------------------------------------------------------- 构建后处理

const dist = path.join(repoRoot, 'dist');
const distFiles = fs.readdirSync(dist);

// 插件入口自检：plugin.json 的 entry 必须真的存在
const entry = path.join(dist, 'index.html');
if (!fs.existsSync(entry)) {
  console.error(`构建产物里没有 ${entry}，但 plugin.json 的 entry 指向它。`);
  process.exit(1);
}

// 渲染 worker：上游 worker.js 从 `./lightcraft_web.js` 导入 wasm 胶水；trunk 开
// filehash 后胶水叫 `otools-lightcraft-<哈希>.js`，构建后替换成实际文件名。
const glue = distFiles.find((f) => f.startsWith('otools-lightcraft-') && f.endsWith('.js'));
if (!glue) {
  console.error('dist/ 里找不到 wasm 胶水 JS（otools-lightcraft-*.js），渲染 worker 无法就位。');
  process.exit(1);
}
const wasmName = distFiles.find((f) => f.startsWith('otools-lightcraft-') && f.endsWith('_bg.wasm'));
if (!wasmName) {
  console.error('dist/ 里找不到 wasm 模块（otools-lightcraft-*_bg.wasm）。');
  process.exit(1);
}
const workerTemplate = fs.readFileSync(path.join(repoRoot, 'shell', 'worker.js'), 'utf8');
fs.writeFileSync(path.join(dist, 'worker.js'), workerTemplate.replaceAll('./lightcraft_web.js', `./${glue}`));

// 上游 Workers::start 从 `window.lightcraftModule` 取编译好的模块起渲染 worker；
// trunk 的加载器在 init 内部编译，拿不到模块。给加载器打补丁：先 compileStreaming
// 预编译并挂到 window，再把模块交给 init（wasm-bindgen 接受 WebAssembly.Module）。
// trunk 生成的调用是 `const wasm = await init({...})`，所以这里用 IIFE 保住赋值。
const indexPath = path.join(dist, 'index.html');
const index = fs.readFileSync(indexPath, 'utf8');
const initCall = `await init({ module_or_path: './${wasmName}' })`;
const initCallPatched = `await (async () => {\n` +
  `  window.lightcraftModule = await WebAssembly.compileStreaming(fetch('./${wasmName}'));\n` +
  `  return init({ module_or_path: window.lightcraftModule });\n` +
  `})()`;
if (!index.includes(initCall)) {
  console.error(`dist/index.html 里找不到 trunk 的 init 调用（${initCall}），worker 模块补丁未打上。`);
  process.exit(1);
}
fs.writeFileSync(indexPath, index.replace(initCall, initCallPatched));

// 中文字体 → dist/fonts/（运行时 fetch；缺失只警告）
const fontCache = path.join(repoRoot, 'fonts', 'cache');
const fonts = [];
if (fs.existsSync(fontCache)) {
  for (const commit of fs.readdirSync(fontCache)) {
    for (const name of fs.readdirSync(path.join(fontCache, commit))) {
      fonts.push({ from: path.join(fontCache, commit, name), name });
    }
  }
}
if (fonts.length) {
  fs.mkdirSync(path.join(dist, 'fonts'), { recursive: true });
  for (const f of fonts) {
    fs.copyFileSync(f.from, path.join(dist, 'fonts', f.name));
  }
  console.log(`▸ 字体就位：${fonts.map((f) => `fonts/${f.name}`).join(', ')}`);
} else {
  console.warn('! fonts/cache 为空：dist/fonts 里没有中文字体，中日韩界面文字会显示方框。');
}

const size = (file) => `${(fs.statSync(file).size / 1048576).toFixed(1)} MB`;
const listDir = (dir, prefix = '') => {
  for (const name of fs.readdirSync(dir)) {
    const file = path.join(dir, name);
    if (fs.statSync(file).isDirectory()) listDir(file, `${prefix}${name}/`);
    else console.log(`  ${prefix}${name}  ${size(file)}`);
  }
};
listDir(dist);
console.log(`\n完成。${entry} 就绪；用 scripts/deploy-to-otools.ps1 同步到 otools 仓库即可本地试跑。`);
