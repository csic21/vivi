/** 信令地址：构建时 VITE_SIGNALING_URL 为缺省，运行时可在首页覆盖（跨机联调）。
 *
 * 跨机原理：房间只存在于某台机器的信令内存里，两台必须连同一个信令
 * （都填房主的 http://<房主IP>:8080），否则各连各的本机，永远碰不上面。
 */

const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
  .env;

export const DEFAULT_SIGNALING_HTTP: string =
  env?.VITE_SIGNALING_URL ?? "http://127.0.0.1:8080";

const OVERRIDE_KEY = "gamevoice.signalingHttp";

function normalizeHttp(raw: string): string {
  return raw.trim().replace(/\/+$/, "");
}

export function getSignalingHttp(): string {
  try {
    const saved = localStorage.getItem(OVERRIDE_KEY);
    if (saved && saved.trim()) return normalizeHttp(saved);
  } catch {
    /* localStorage 不可用时回退默认值 */
  }
  return DEFAULT_SIGNALING_HTTP;
}

export function setSignalingHttp(url: string): string {
  const next = normalizeHttp(url);
  try {
    localStorage.setItem(OVERRIDE_KEY, next);
  } catch {
    /* 忽略持久化失败，本次会话仍用新值 */
  }
  return next;
}

/** 由 HTTP 地址派生 WS 地址（http→ws，https→wss）。 */
export function wsFromHttp(http: string): string {
  return normalizeHttp(http).replace(/^http/, "ws") + "/signal";
}

/** 兼容旧引用：模块加载时的快照；新代码请用 getSignalingHttp()/wsFromHttp()。 */
export const SIGNALING_HTTP: string = DEFAULT_SIGNALING_HTTP;
export const SIGNALING_WS: string = wsFromHttp(DEFAULT_SIGNALING_HTTP);
