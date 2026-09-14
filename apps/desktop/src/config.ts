/** 信令地址：VITE_SIGNALING_URL 覆盖，缺省本地开发值。 */

const env = (import.meta as unknown as { env?: Record<string, string | undefined> })
  .env;

export const SIGNALING_HTTP: string =
  env?.VITE_SIGNALING_URL ?? "http://127.0.0.1:8080";

export const SIGNALING_WS: string =
  env?.VITE_SIGNALING_WS ??
  SIGNALING_HTTP.replace(/^http/, "ws") + "/signal";
