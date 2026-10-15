/**
 * 按 fonts.pin 下载插件需要的字体到 fonts/cache/（gitignored）。
 * ------------------------------------------------------------------
 * LightCraft 的界面有 14 种语言，中日韩字形依赖 Noto Sans CJK SC。上游网页版把它作为
 * 独立文件随站点分发（嵌进 wasm 会超宿主体积上限，见上游 xtask/src/web.rs），本插件照抄：
 * `node scripts/build-wasm.mjs` 会先跑这个脚本，再把字体拷进 dist/fonts/。
 *
 * 只按 pinned commit + sha256 取单个文件（craft-fonts 整库 ~68 MB，没必要克隆）；
 * 已存在且哈希一致时跳过下载。`OT_FONTS_SRC` 环境变量可指向本地字体目录（离线环境）。
 *
 * 用法：node scripts/fetch-fonts.mjs [--force]
 */
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const force = process.argv.includes('--force');
const pin = JSON.parse(fs.readFileSync(path.join(repoRoot, 'fonts.pin'), 'utf8'));
const cacheDir = path.join(repoRoot, 'fonts', 'cache', pin.commit.slice(0, 12));

if (!/^[0-9a-f]{40}$/.test(pin.commit ?? '')) {
  console.error('fonts.pin 的 commit 必须是 40 位完整 SHA（否则无法保证可复现）');
  process.exit(1);
}

const sha256 = (file) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');

/** 本地缓存已对齐（哈希一致）就跳过。 */
function cached(file, expectedSha) {
  if (!fs.existsSync(file)) return false;
  try { return sha256(file) === expectedSha; } catch { return false; }
}

/** 网络获取（raw.githubusercontent.com 直连；失败重试一次）。 */
async function download(url, attempt = 1) {
  try {
    const res = await fetch(url, { redirect: 'follow', signal: AbortSignal.timeout(120000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return Buffer.from(await res.arrayBuffer());
  } catch (e) {
    if (attempt >= 2) throw new Error(`下载失败（${e.message}）：${url}`);
    console.warn(`  第 ${attempt} 次失败（${e.message}），重试…`);
    return download(url, attempt + 1);
  }
}

let failed = 0;
fs.mkdirSync(cacheDir, { recursive: true });
for (const font of pin.fonts ?? []) {
  const target = path.join(cacheDir, font.out);
  const licenceTarget = path.join(cacheDir, font.licenceOut);
  if (!force && cached(target, font.sha256) && fs.existsSync(licenceTarget)) {
    console.log(`字体已缓存：${font.family} ${font.style} → ${path.relative(repoRoot, target)}`);
    continue;
  }
  const base = `https://raw.githubusercontent.com/${pin.upstream}/${pin.commit}`;
  try {
    // 离线环境可用 OT_FONTS_SRC=<本地 craft-fonts checkout> 直接取文件。
    const local = process.env.OT_FONTS_SRC ? path.join(process.env.OT_FONTS_SRC, font.file) : null;
    if (local && fs.existsSync(local)) {
      fs.copyFileSync(local, target);
    } else {
      console.log(`▸ 下载 ${font.file} …`);
      const data = await download(`${base}/${font.file}`);
      fs.writeFileSync(target, data);
      if (sha256(target) !== font.sha256) {
        fs.rmSync(target);
        throw new Error(`sha256 不符（期望 ${font.sha256}）`);
      }
    }
    if (process.env.OT_FONTS_SRC && fs.existsSync(path.join(process.env.OT_FONTS_SRC, font.licence))) {
      fs.copyFileSync(path.join(process.env.OT_FONTS_SRC, font.licence), licenceTarget);
    } else {
      const licence = await download(`${base}/${font.licence}`);
      fs.writeFileSync(licenceTarget, licence);
    }
    console.log(`✓ ${font.family} ${font.style} → ${path.relative(repoRoot, target)}（${(fs.statSync(target).size / 1048576).toFixed(1)} MB）`);
  } catch (e) {
    console.error(`  ! ${font.family} ${font.style}：${e.message}`);
    failed += 1;
  }
}

if (failed) {
  console.error('字体就位失败：界面里的中日韩文字会显示方框（不影响启动）。');
  console.error('可设置 OT_FONTS_SRC 指向本地 craft-fonts 检出后重试。');
  process.exit(1);
}
