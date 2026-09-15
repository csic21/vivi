#!/usr/bin/env node
/**
 * 为 Tauri updater 生成 latest.json 并上传到 GitHub Release。
 *
 * 前置条件（由 release-desktop.yml 保证）：
 *   1. tauri-action 已把各平台安装包 + .sig 签名文件上传到同一 Release；
 *   2. 环境里有 gh CLI 且已登录（Actions 里默认可用，GITHUB_TOKEN 授权）。
 *
 * 用法：
 *   TAG=v0.2.0 REPO=csic21/vivi node apps/desktop/scripts/gen-latest-json.mjs
 *   TAG=v0.2.0 REPO=csic21/vivi NOTES_FILE=/tmp/notes.md node ...  # 自定义更新说明
 *
 * latest.json 格式（Tauri v2 updater 约定）：
 *   { version, notes, pub_date, platforms: { <rid>: { signature, url } } }
 * 没构建的平台不会有 key（比如只打了 arm64 mac 包，x64 mac 键就不会出现），
 * 但**有产物却认不出来**会直接报错退出 —— 那说明规则该补了，别让它悄悄少一个平台。
 */
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const TAG = process.env.TAG || process.argv[2];
const REPO = process.env.REPO || "csic21/vivi";
if (!TAG) {
  console.error("用法：TAG=v0.2.0 REPO=csic21/vivi node gen-latest-json.mjs");
  process.exit(1);
}
const VERSION = TAG.startsWith("v") ? TAG.slice(1) : TAG;

/** release 安装包文件名 -> [{ rid, bundle }]。
 *
 * 客户端（@tauri-apps/plugin-updater v2）按 `{os}-{arch}-{installer}` → `{os}-{arch}`
 * 顺序找 key，installer 取自本机安装方式（nsis/msi/appimage/deb/rpm/app）。
 * 所以每个包既写主 key（兼容），也写带安装包后缀的变体 key（精准路由）。
 *
 * 两种产物形状都要认：
 * - 旧式 zipped（Tauri 早期 v2）：`x64-setup.nsis.zip`、`amd64.AppImage.tar.gz`；
 * - 新式 unzipped（当前 CLI 直接给安装包签名）：`x64-setup.exe`、`.msi`、`.AppImage`。
 */
function entriesOf(bundle) {
  if (/x64-setup\.nsis\.zip$/.test(bundle))
    return [{ rid: "windows-x86_64", bundle: "nsis" }];
  if (/x64-setup\.exe$/.test(bundle))
    return [{ rid: "windows-x86_64", bundle: "nsis" }];
  if (/x64_[a-zA-Z-]+\.msi$/.test(bundle) || /x64_[a-zA-Z-]+\.msi\.zip$/.test(bundle))
    return [{ rid: "windows-x86_64", bundle: "msi" }];
  if (/x64\.app\.tar\.gz$/.test(bundle))
    return [{ rid: "darwin-x86_64", bundle: "app" }];
  if (/aarch64\.app\.tar\.gz$/.test(bundle))
    return [{ rid: "darwin-aarch64", bundle: "app" }];
  if (/amd64\.AppImage(\.tar\.gz)?$/.test(bundle))
    return [{ rid: "linux-x86_64", bundle: "appimage" }];
  if (/aarch64\.AppImage(\.tar\.gz)?$/.test(bundle))
    return [{ rid: "linux-aarch64", bundle: "appimage" }];
  if (/amd64\.deb$/.test(bundle)) return [{ rid: "linux-x86_64", bundle: "deb" }];
  if (/x86_64\.rpm$/.test(bundle)) return [{ rid: "linux-x86_64", bundle: "rpm" }];
  return [];
}

function gh(...args) {
  return execFileSync("gh", [...args, "--repo", REPO], { encoding: "utf8" });
}

const assets = JSON.parse(gh("release", "view", TAG, "--json", "assets")).assets;
const byName = new Map(assets.map((a) => [a.name, a]));

const platforms = {};
/** 主 key 已被谁占用（windows 主 key 留给 nsis，linux 留给 appimage）。
 *
 * windows 这里**故意和 tauri-action 的默认值（msi）相反**：主 key 只在客户端
 * `bundle_type()` 读不出来时才兜底，而 NSIS/MSI 在 Windows 上是两套独立的卸载记录
 * —— 拿 MSI 去覆盖 NSIS 装的旧版不会替换，而是并排再装一份。
 * 用户从 Release 页下载的绝大多数是 `-setup.exe`，所以兜底也兜 NSIS。
 * 正常装好的客户端不用兜底：它会命中 windows-x86_64-nsis / -msi。
 */
const primaryTaken = new Set();
function primaryRank(rid, bundle) {
  if (rid === "windows-x86_64") return bundle === "nsis" ? 0 : 1;
  if (rid === "linux-x86_64" || rid === "linux-aarch64")
    return bundle === "appimage" ? 0 : 1;
  return 0;
}
const pending = [];
/** 有 .sig 却不认识的产物：这是漏平台的前兆，默认直接让 CI 挂掉（见文件末尾）。 */
const unrecognized = [];
for (const [name, asset] of byName) {
  if (!name.endsWith(".sig")) continue;
  const bundle = name.slice(0, -".sig".length);
  const target = byName.get(bundle);
  if (!target) {
    unrecognized.push(`${name}（找不到对应的安装包 ${bundle}）`);
    continue;
  }
  const entries = entriesOf(bundle);
  if (entries.length === 0) {
    unrecognized.push(bundle);
    continue;
  }
  const signature = gh("release", "download", TAG, "--pattern", name, "--output", "-").trim();
  // **不要**用 `gh release view` 给的 assets[].url。Release 还是草稿时（本脚本
  // 就是在草稿阶段跑的，这是设计）那个字段是 `.../download/untagged-<hash>/<file>`
  // ——草稿还没有 tag；转正之后这个地址就不解析了，客户端全员 404。
  // 直接用 tag 拼，草稿和正式拿到的是同一个地址。verifier 里有对应断言兜底。
  const url = `https://github.com/${REPO}/releases/download/${TAG}/${encodeURIComponent(bundle)}`;
  for (const { rid, bundle: kind } of entries) {
    // 变体 key：精准路由到同安装方式的客户端
    platforms[`${rid}-${kind}`] = { signature, url };
    pending.push({ rid, kind, signature, url });
  }
}
// 主 key：同 rid 多包时按优先级只留一个（windows 优先 nsis，linux 优先 appimage）
pending
  .sort((a, b) => primaryRank(a.rid, a.kind) - primaryRank(b.rid, b.kind))
  .forEach(({ rid, signature, url }) => {
    if (!primaryTaken.has(rid)) {
      primaryTaken.add(rid);
      platforms[rid] = { signature, url };
    }
  });

// 认不出来的 .sig 一律当错误处理：v0.3.0/v0.3.1 就是在这里静默漏掉整个 windows 平台，
// CI 全绿、客户端却永远"检查不到更新"，只能靠人肉发现。宁可发版失败也不要发坏清单。
if (unrecognized.length > 0 && process.env.ALLOW_UNRECOGNIZED_SIG !== "1") {
  console.error("以下带签名的产物无法映射到任何平台，latest.json 会缺平台：");
  for (const item of unrecognized) console.error(`  - ${item}`);
  console.error("确认是无关产物就设 ALLOW_UNRECOGNIZED_SIG=1 重跑；否则补 entriesOf 的规则。");
  process.exit(1);
}

if (Object.keys(platforms).length === 0) {
  console.error("没有找到任何带 .sig 的 updater 产物，latest.json 无法生成。");
  console.error("请确认 tauri.conf.json 里 createUpdaterArtifacts=true 且签名密钥已配置。");
  process.exit(1);
}

let notes = `Vivi ${TAG}`;
try {
  const f = process.env.NOTES_FILE;
  if (f) notes = readFileSync(f, "utf8").trim() || notes;
} catch {
  /* 忽略，使用默认文案 */
}

const latest = {
  version: VERSION,
  notes,
  pub_date: new Date().toISOString(),
  platforms,
};

const out = join(tmpdir(), "latest.json");
writeFileSync(out, `${JSON.stringify(latest, null, 2)}\n`);
console.log(`平台：${Object.keys(platforms).join(", ")}`);
console.log(JSON.stringify(latest, null, 2).slice(0, 800));

execFileSync("gh", ["release", "upload", TAG, out, "--clobber", "--repo", REPO], { stdio: "inherit" });
console.log(`\nlatest.json 已上传到 ${TAG} ✓`);
