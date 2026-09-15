#!/usr/bin/env node
/**
 * 发版后自检：确认公网 endpoint 上的 latest.json 真的是本次 tag、且平台齐全。
 *
 * 为什么需要：latest.json 是客户端唯一的更新来源，它没有版本历史、也看不出来"少了一截"。
 * v0.3.0 / v0.3.1 两次发版的流水线全绿，但清单里一个 windows-* key 都没有，
 * 客户端只会静默地"检查不到更新"——靠人肉发现花了一个小时。
 * 这个脚本跑在 `gh release edit --draft=false` 之后（必须转正后才能走 releases/latest），
 * 任何一项对不上就退出非零，让 Release 页面直接标红。
 *
 * 用法：
 *   TAG=v0.3.1 REPO=csic21/vivi node apps/desktop/scripts/verify-latest-json.mjs
 *   TIMEOUT_SECS=300 INTERVAL_SECS=15 node ...   # 调轮询时长/间隔
 */
import { execFileSync } from "node:child_process";

const TAG = process.env.TAG || process.argv[2];
const REPO = process.env.REPO || "csic21/vivi";
if (!TAG) {
  console.error("用法：TAG=v0.3.1 REPO=csic21/vivi node verify-latest-json.mjs");
  process.exit(1);
}
const VERSION = TAG.startsWith("v") ? TAG.slice(1) : TAG;
const TIMEOUT_SECS = Number(process.env.TIMEOUT_SECS || 180);
const INTERVAL_SECS = Number(process.env.INTERVAL_SECS || 10);
// 走 releases/latest 而不是直连 tag：客户端用的就是这个 URL，
// 顺带验证 "latest 指针已经翻到本次 tag"（发布刚完成时会有延迟）。
const ENDPOINT = `https://github.com/${REPO}/releases/latest/download/latest.json`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function gh(...args) {
  return execFileSync("gh", [...args, "--repo", REPO], { encoding: "utf8" });
}

/** 本次 Release 实际构建了哪些平台族 —— 清单必须每个族至少有一个 key。 */
function expectedFamilies(assetNames) {
  const families = new Set();
  for (const name of assetNames) {
    if (/x64[-_].*(setup\.exe|\.msi)$/i.test(name)) families.add("windows");
    if (/\.app\.tar\.gz$/i.test(name)) families.add("darwin");
    if (/\.(AppImage|deb|rpm)$/i.test(name)) families.add("linux");
  }
  return families;
}

async function fetchManifest() {
  const res = await fetch(ENDPOINT, { redirect: "follow" });
  if (!res.ok) return { error: `HTTP ${res.status}` };
  try {
    return { manifest: await res.json() };
  } catch (e) {
    return { error: `JSON 解析失败：${e.message}` };
  }
}

/** 逐个 HEAD 一下产物地址：签名对但 URL 404 一样是白搭。 */
async function checkUrl(url) {
  try {
    const res = await fetch(url, { method: "HEAD", redirect: "follow" });
    return res.ok ? null : `HTTP ${res.status}`;
  } catch (e) {
    return e.message;
  }
}

async function main() {
  // 草稿状态直接说清楚：draft 不进 releases/latest，轮询再久也等不到。
  const release = JSON.parse(gh("release", "view", TAG, "--json", "isDraft,assets"));
  if (release.isDraft) {
    console.error(`✗ Release ${TAG} 还是草稿状态，releases/latest 不会指向它。`);
    console.error("  先 gh release edit ${TAG} --draft=false 再跑本脚本。");
    process.exit(1);
  }

  const families = expectedFamilies(release.assets.map((a) => a.name));
  if (families.size === 0) {
    console.error(`✗ ${TAG} 里没找到任何可识别的安装包，Release 本身有问题。`);
    process.exit(1);
  }
  console.log(`期望平台族：${[...families].join(", ")}`);

  const deadline = Date.now() + TIMEOUT_SECS * 1000;
  let manifest = null;
  for (;;) {
    const { manifest: got, error } = await fetchManifest();
    if (got) {
      if (got.version === VERSION) {
        manifest = got;
        break;
      }
      // 版本对不上通常是 latest 还指着上一个 Release，等它翻。
      console.log(`… endpoint 当前是 v${got.version}，等它变成 v${VERSION}`);
    } else {
      console.log(`… ${error}（latest.json 可能还没传播开）`);
    }
    if (Date.now() >= deadline) {
      console.error(`✗ 等了 ${TIMEOUT_SECS}s，${ENDPOINT} 仍未返回 v${VERSION}。`);
      console.error("  检查：Release 是否已转正、latest.json 是否上传成功。");
      process.exit(1);
    }
    await sleep(INTERVAL_SECS * 1000);
  }

  const keys = Object.keys(manifest.platforms || {});
  console.log(`endpoint 返回 v${manifest.version}，平台 key：${keys.join(", ")}`);

  const problems = [];

  // 1) 平台齐全：漏一整个族正是那次翻车的样子。
  for (const family of families) {
    if (!keys.some((k) => k.startsWith(`${family}-`))) {
      problems.push(`缺少 ${family}-* 的 key（本次构建产出里明明有该平台产物）`);
    }
  }

  // 2) 每个 key 形状正确
  for (const key of keys) {
    const entry = manifest.platforms[key];
    if (!entry?.url || !entry?.signature) problems.push(`${key}：缺 url 或 signature`);
  }

  // 3) 产物真的下得动
  for (const key of keys) {
    const entry = manifest.platforms[key];
    if (!entry?.url) continue;
    const bad = await checkUrl(entry.url);
    if (bad) problems.push(`${key}：${entry.url.split("/").pop()} 不可访问（${bad}）`);
  }

  if (problems.length > 0) {
    console.error("\n✗ latest.json 自检未通过：");
    for (const p of problems) console.error(`  - ${p}`);
    process.exit(1);
  }
  console.log(`\n✓ ${ENDPOINT} 已指向 v${VERSION}，${keys.length} 个平台 key 全部可用。`);
}

main().catch((e) => {
  console.error(`✗ 自检脚本自身出错：${e.stack || e}`);
  process.exit(1);
});
