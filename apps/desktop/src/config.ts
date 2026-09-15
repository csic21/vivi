/** 信令地址解析。
 *
 * 为什么会变成这样：以前信令地址是"必填项"，默认值还是没用的 `127.0.0.1:8080`，
 * 新手完全不知道该填什么（真实事故：Windows 建房、Mac 保留默认值 → 各连各的本机
 * → "房间不存在"）。现在默认走自动：
 *
 *   加入：手动 > 自动发现 > 本机内嵌 > 构建期默认
 *   建房：手动 > 本机内嵌 > 构建期默认
 *
 * 建房**不能**用自动发现的地址：那个地址可能指向别人的机器，会把房间建到别人那台去。
 *
 * 房间只存在于某台机器的信令进程内存里，所以两台必须连同一个信令。
 * 同一局域网靠 mDNS 自动发现（Rust 侧 discovery.rs），异地靠邀请串。
 */

const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
  .env;

/** 构建期缺省。注：`VITE_SIGNALING_URL` 是**构建期**变量，改它要重新打包；
 *  运行时改地址走首页的「高级 → 连接设置」。 */
export const DEFAULT_SIGNALING_HTTP: string =
  env?.VITE_SIGNALING_URL ?? "http://127.0.0.1:8080";

const OVERRIDE_KEY = "vivi.signalingHttp";
const AUTO_KEY = "vivi.signalingAuto";
const LEGACY_OVERRIDE_KEY = "gamevoice.signalingHttp";

function normalizeHttp(raw: string): string {
  return raw.trim().replace(/\/+$/, "");
}

function read(key: string): string | null {
  try {
    const v = localStorage.getItem(key);
    return v && v.trim() ? normalizeHttp(v) : null;
  } catch {
    return null;
  }
}

function write(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* localStorage 不可用：本次会话仍能用内存态，忽略持久化失败 */
  }
}

/** 本机内嵌信令的实际地址，由 Rust 的 `get_signaling_status()` 在启动时灌进来。
 *  在它到达之前用构建期默认值兜底。 */
let localHttp: string | null = null;

export function setLocalSignalingHttp(url: string | null): void {
  const next = url ? normalizeHttp(url) : null;
  if (next === localHttp) return;
  localHttp = next;
  notify();
}

/** 本机内嵌信令地址（可能没起来 → 返回构建期默认值）。 */
export function getLocalSignalingHttp(): string {
  return localHttp ?? DEFAULT_SIGNALING_HTTP;
}

/** 加入房间时该连哪个信令。 */
export function getSignalingHttp(): string {
  return read(OVERRIDE_KEY) ?? read(AUTO_KEY) ?? localHttp ?? DEFAULT_SIGNALING_HTTP;
}

/** 建房时该连哪个信令 —— 只认本机，绝不落到自动发现的别人那台上。 */
export function getCreateSignalingHttp(): string {
  return read(OVERRIDE_KEY) ?? localHttp ?? DEFAULT_SIGNALING_HTTP;
}

export type SignalingMode = "manual" | "auto" | "local";

export function getSignalingMode(): SignalingMode {
  if (read(OVERRIDE_KEY)) return "manual";
  if (read(AUTO_KEY)) return "auto";
  return "local";
}

/** 用户手填地址：保存后手动优先，同时清掉自动发现留下的地址。 */
export function setSignalingHttp(url: string): string {
  const next = normalizeHttp(url);
  write(OVERRIDE_KEY, next);
  write(AUTO_KEY, null);
  notify();
  return next;
}

/** 自动发现写入的地址。不覆盖手填设置，否则一次自动发现会永久污染用户的选择。 */
export function setAutoSignalingHttp(url: string): string {
  const next = normalizeHttp(url);
  write(AUTO_KEY, next);
  notify();
  return next;
}

/** 丢掉自动发现的地址；手动设置也一并清掉，回到"自动"。 */
export function clearSignalingOverride(): void {
  write(OVERRIDE_KEY, null);
  write(AUTO_KEY, null);
  notify();
}

/** 兼容旧版 GameVoice 存的地址：读到就迁移到新 key。启动时调一次。 */
export function migrateLegacySignalingHttp(): void {
  if (read(OVERRIDE_KEY)) return;
  const legacy = read(LEGACY_OVERRIDE_KEY);
  if (!legacy) return;
  write(OVERRIDE_KEY, legacy);
  write(LEGACY_OVERRIDE_KEY, null);
}

// ---------- 订阅（给 useSyncExternalStore 用） ----------

const listeners = new Set<() => void>();

export function subscribeSignaling(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

function notify(): void {
  for (const cb of listeners) cb();
}

/** 由 HTTP 地址派生 WS 地址（http→ws，https→wss）。 */
export function wsFromHttp(http: string): string {
  return normalizeHttp(http).replace(/^http/, "ws") + "/signal";
}
