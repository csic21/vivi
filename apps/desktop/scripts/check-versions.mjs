#!/usr/bin/env node
/**
 * 版本一致性检查：发版 tag (vX.Y.Z) 必须与以下四处完全一致，
 * 否则桌面端更新器 / 安装包版本号会互相打架。
 *   - Cargo.toml                [workspace.package] version
 *   - apps/desktop/package.json version
 *   - apps/desktop/src-tauri/Cargo.toml  version
 *   - apps/desktop/src-tauri/tauri.conf.json version
 *
 * 用法：
 *   node apps/desktop/scripts/check-versions.mjs            # 只检查四处一致
 *   node apps/desktop/scripts/check-versions.mjs v0.2.0     # 额外校验 tag 匹配
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function readJson(path) {
  return JSON.parse(readFileSync(join(root, path), "utf8"));
}

function readCargoVersion(path) {
  const text = readFileSync(join(root, path), "utf8");
  // 根 Cargo.toml 取 [workspace.package] 段；子 crate 取 [package] 段
  const section = path === "Cargo.toml" ? "workspace.package" : "package";
  const secIdx = text.indexOf(`[${section}]`);
  if (secIdx === -1) throw new Error(`${path}: missing [${section}]`);
  const tail = text.slice(secIdx);
  const m = tail.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error(`${path}: version not found in [${section}]`);
  return m[1];
}

const versions = new Map([
  ["Cargo.toml [workspace.package]", readCargoVersion("Cargo.toml")],
  ["apps/desktop/package.json", readJson("apps/desktop/package.json").version],
  ["apps/desktop/src-tauri/Cargo.toml", readCargoVersion("apps/desktop/src-tauri/Cargo.toml")],
  ["apps/desktop/src-tauri/tauri.conf.json", readJson("apps/desktop/src-tauri/tauri.conf.json").version],
]);

for (const [where, v] of versions) console.log(`${v}  ${where}`);

const unique = new Set(versions.values());
if (unique.size !== 1) {
  console.error("\n版本不一致！发版前请把四处改成同一个版本号。");
  process.exit(1);
}

const tag = process.argv[2];
if (tag) {
  const expected = tag.startsWith("v") ? tag.slice(1) : tag;
  const actual = [...unique][0];
  if (expected !== actual) {
    console.error(`\ntag ${tag} 与代码版本 ${actual} 不一致！`);
    process.exit(1);
  }
}

console.log("\n版本一致 ✓");
