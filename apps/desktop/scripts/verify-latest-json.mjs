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

  // 整段都放进轮询里，包括产物可达性。
  //
  // 为什么不能只等版本号：`--draft=false` 之后 GitHub 各条路径的传播时间不一样。
  // v0.4.0 这次就是——`releases/latest/download/latest.json` 已经返回新版本了，
  // 但同一个 Release 里直接走 tag 的 `releases/download/v0.4.0/<包>` 还在 404，
  // 几秒后才好。单次判定会把这种"暂时没传播开"误报成"产物挂了"。
  const deadline = Date.now() + TIMEOUT_SECS * 1000;
  let lastProblems = ["（还没拿到 manifest）"];
  let keys = [];
  for (;;) {
    const { manifest: got, error } = await fetchManifest();
    if (got) {
      if (got.version === VERSION) {
        keys = Object.keys(got.platforms || {});
        lastProblems = await inspect(got, keys, families);
        if (lastProblems.length === 0) {
          console.log(
            `\n✓ ${ENDPOINT} 已指向 v${VERSION}，${keys.length} 个平台 key 全部可用。`,
          );
          return;
        }
        console.log(
          `… 版本对了，还差 ${lastProblems.length} 项：${lastProblems[0]}`,
        );
      } else {
        // 版本对不上通常是 latest 还指着上一个 Release，等它翻。
        console.log(`… endpoint 当前是 v${got.version}，等它变成 v${VERSION}`);
      }
    } else {
      console.log(`… ${error}（latest.json 可能还没传播开）`);
    }
    if (Date.now() >= deadline) {
      console.error(`\n✗ 等了 ${TIMEOUT_SECS}s 仍未通过自检：`);
      for (const p of lastProblems) console.error(`  - ${p}`);
      console.error(
        "  逐项排查：Release 是否已转正、latest.json 是否上传成功、" +
          "产物地址是否真的存在。",
      );
      process.exit(1);
    }
    await sleep(INTERVAL_SECS * 1000);
  }
}

/** 检查一份 manifest：平台齐全、字段完整、产物真的下得动。返回问题列表。 */
async function inspect(manifest, keys, families) {
  const problems = [];
  console.log(`endpoint 返回 v${manifest.version}，平台 key：${keys.join(", ")}`);

  // 1) 平台齐全：漏一整个族正是那次翻车的样子。
  for (const family of families) {
    if (!keys.some((k) => k.startsWith(`${family}-`))) {
      problems.push(`缺少 ${family}-* 的 key（本次构建产出里明明有该平台产物）`);
    }
  }

  // 2) 每个 key 形状正确，且地址走的是**本 tag** 而不是草稿期的 untagged-<hash>。
  //    草稿地址在 Release 转正后会 404，客户端一个都下不动——v0.4.0 踩过。
  const expectedPrefix = `https://github.com/${REPO}/releases/download/${TAG}/`;
  for (const key of keys) {
    const entry = manifest.platforms[key];
    if (!entry?.url || !entry?.signature) {
      problems.push(`${key}：缺 url 或 signature`);
      continue;
    }
    if (!entry.url.startsWith(expectedPrefix)) {
      problems.push(`${key}：地址不是本 tag 的路径（草稿期 URL？）—— ${entry.url}`);
    }
  }

  // 3) 产物真的下得动（同一个 url 只探一次，manifest 里多个 key 会指向同一个包）
  const urls = new Set(
    keys.map((k) => manifest.platforms[k]?.url).filter(Boolean),
  );
  for (const url of urls) {
    const bad = await checkUrl(url);
    if (bad) problems.push(`${url.split("/").pop()} 不可访问（${bad}）`);
  }

  return problems;
}

main().catch((e) => {
  console.error(`✗ 自检脚本自身出错：${e.stack || e}`);
  process.exit(1);
});
