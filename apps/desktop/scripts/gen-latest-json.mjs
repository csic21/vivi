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
 * 缺失的平台会被跳过（比如只打了 arm64 mac 包，x64 mac 键就不会出现）。
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

/** release 资产文件名 -> updater platform key */
function platformOf(asset) {
  if (/x64-setup\.nsis\.zip$/.test(asset)) return "windows-x86_64";
  if (/x64\.app\.tar\.gz$/.test(asset)) return "darwin-x86_64";
  if (/aarch64\.app\.tar\.gz$/.test(asset)) return "darwin-aarch64";
  if (/amd64\.AppImage\.tar\.gz$/.test(asset)) return "linux-x86_64";
  if (/aarch64\.AppImage\.tar\.gz$/.test(asset)) return "linux-aarch64";
  return null;
}

function gh(...args) {
  return execFileSync("gh", [...args, "--repo", REPO], { encoding: "utf8" });
}

const assets = JSON.parse(gh("release", "view", TAG, "--json", "assets")).assets;
const byName = new Map(assets.map((a) => [a.name, a]));

const platforms = {};
for (const [name, asset] of byName) {
  if (!name.endsWith(".sig")) continue;
  const bundle = name.slice(0, -".sig".length);
  const target = byName.get(bundle);
  if (!target) {
    console.warn(`跳过 ${name}：找不到对应的安装包 ${bundle}`);
    continue;
  }
  const rid = platformOf(bundle);
  if (!rid) {
    console.warn(`跳过 ${bundle}：无法识别平台`);
    continue;
  }
  const signature = gh("release", "download", TAG, "--pattern", name, "--output", "-").trim();
  platforms[rid] = { signature, url: target.url };
}

if (Object.keys(platforms).length === 0) {
  console.error("没有找到任何带 .sig 的 updater 产物，latest.json 无法生成。");
  console.error("请确认 tauri.conf.json 里 createUpdaterArtifacts=true 且签名密钥已配置。");
  process.exit(1);
}

let notes = `GameVoice ${TAG}`;
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
