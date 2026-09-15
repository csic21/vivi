/** 信令 HTTP：建房 + 查房 + TURN 凭证（webview 直调，CORS 已放行）。
 *
 * 这里同时负责"该连哪个信令"的解析：手动 > 当前地址 > 局域网发现。
 * 房间只存在于某台机器的信令进程内存里，所以两台连不上同一个信令就永远碰不上面。
 */
import {
  getCreateSignalingHttp,
  getSignalingHttp,
  getSignalingMode,
  setAutoSignalingHttp,
  setLocalSignalingHttp,
  wsFromHttp,
} from "../config";
import { ipc } from "../ipc";
import type { DiscoveredServer, TurnConfig } from "../types";

/** 单点探测超时。局域网内正常是个位数毫秒；指向被防火墙丢包的地址时，
 *  不设超时会挂到 TCP 超时（几十秒），多候选并发时体验会很差。 */
const PROBE_TIMEOUT_MS = 900;
/** mDNS 浏览时长：冷启动要等一次多播往返，命中缓存则几十毫秒就返回。 */
const DISCOVER_TIMEOUT_MS = 2500;

/** 发现不到人时给用户的下一步。多处复用，避免文案漂移。 */
export const DISCOVERY_HINT =
  "确认两台在同一个 Wi-Fi（很多路由器的访客网络 / AP 隔离会挡住发现），" +
  "或者让房主点「邀请」把地址发给你，粘进房间号框。";

/** 房号规范化：去空格转小写（建房产出 8 位小写 hex，手动输入常带空格/大写）。 */
export function normalizeRoomId(raw: string): string {
  return raw.trim().toLowerCase();
}

async function fetchWithTimeout(
  url: string,
  ms: number,
  init?: RequestInit,
): Promise<Response> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), ms);
  try {
    return await fetch(url, { ...init, signal: ctl.signal });
  } finally {
    clearTimeout(timer);
  }
}

export async function createRoomAt(base: string): Promise<string> {
  const res = await fetchWithTimeout(
    `${base}/rooms`,
    PROBE_TIMEOUT_MS * 3,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ owner: null }),
    },
  );
  if (!res.ok) throw new Error(`建房失败（信令返回 ${res.status}）`);
  const data = (await res.json()) as { room_id: string };
  return normalizeRoomId(data.room_id);
}

/**
 * 建房：**永远建在本机信令上**。
 *
 * 不能用 `getSignalingHttp()`——那个可能指向自动发现到的别人那台，
 * 会把房间建到人家机器上去。
 */
export async function createRoom(): Promise<string> {
  const base = getCreateSignalingHttp();
  try {
    return await createRoomAt(base);
  } catch (e) {
    throw new Error(await localSignalingDownMessage(base, e));
  }
}

/**
 * 启动时把本机内嵌信令的实际地址告诉 config。
 *
 * Rust 侧的信令是异步起的，刚启动就问很可能还没绑好端口（`port: null`），
 * 所以重试几轮。一直拿不到就算了——`getLocalSignalingHttp()` 会退回构建期默认值。
 */
export async function bootstrapLocalSignaling(
  retries = 10,
  gapMs = 300,
): Promise<void> {
  for (let i = 0; i < retries; i++) {
    try {
      const status = await ipc.getSignalingStatus();
      if (status.local_http) {
        setLocalSignalingHttp(status.local_http);
        return;
      }
    } catch {
      // 非 Tauri 环境（浏览器预览）没有这个 command，直接放弃
      return;
    }
    await new Promise((r) => setTimeout(r, gapMs));
  }
}

export type RoomProbe = "found" | "not-found" | "unreachable";

/** 探某个地址上有没有这个房间。404 = 信令在但没这个房；连不上 = 地址不对。 */
export async function probeRoom(
  base: string,
  roomId: string,
): Promise<RoomProbe> {
  try {
    const res = await fetchWithTimeout(
      `${base}/rooms/${encodeURIComponent(roomId)}`,
      PROBE_TIMEOUT_MS,
    );
    if (res.ok) return "found";
    if (res.status === 404) return "not-found";
    return "unreachable";
  } catch {
    return "unreachable";
  }
}

/**
 * 预检 + 解析：确认能连到"有这个房间"的信令，并把地址落成当前自动地址。
 *
 * 顺序：手动模式下直接用填的地址；否则先试当前地址（快路径，命中就不用等
 * mDNS 那 2.5 秒），不中再做一次局域网发现、并发探所有候选。
 */
export async function ensureRoomReachable(roomId: string): Promise<string> {
  const rid = normalizeRoomId(roomId);

  if (getSignalingMode() === "manual") {
    const base = getSignalingHttp();
    const r = await probeRoom(base, rid);
    if (r === "found") return base;
    if (r === "not-found") throw new Error(roomMissingMessage(rid, []));
    throw new Error(manualUnreachableMessage(base));
  }

  // 快路径
  const current = getSignalingHttp();
  if ((await probeRoom(current, rid)) === "found") return current;

  // 慢路径：局域网发现
  let servers: DiscoveredServer[] = [];
  let discoveryError: string | null = null;
  try {
    const res = await ipc.discoverSignaling(DISCOVER_TIMEOUT_MS);
    servers = res.servers;
    discoveryError = res.error;
  } catch (e) {
    discoveryError = String(e);
  }

  const bases = servers
    .map((s) => (s.addresses[0] ? `http://${s.addresses[0]}:${s.port}` : null))
    .filter((b): b is string => b !== null);

  // 并发探所有候选：串行的话有 N 台就等 N 个超时
  const results = await Promise.all(bases.map((b) => probeRoom(b, rid)));
  const hit = results.indexOf("found");
  if (hit >= 0) {
    setAutoSignalingHttp(bases[hit]);
    return bases[hit];
  }

  if (bases.length === 0) {
    throw new Error(await noDiscoveryMessage(discoveryError));
  }
  throw new Error(roomMissingMessage(rid, servers));
}

/** 当前信令对应的 WS 地址（透传给 Rust join_room，保证 HTTP 与 WS 同源）。 */
export function currentSignalingWs(): string {
  return wsFromHttp(getSignalingHttp());
}

/** 拿 TURN 临时凭证；失败返回 null（纯 P2P 开黑，不阻塞入会）。 */
export async function fetchTurnCreds(userId: number): Promise<TurnConfig | null> {
  try {
    const res = await fetchWithTimeout(
      `${getSignalingHttp()}/turn/credentials?user=${userId}`,
      PROBE_TIMEOUT_MS,
    );
    if (!res.ok) return null;
    const data = (await res.json()) as {
      urls: string[];
      username: string;
      credential: string;
    };
    // 服务端没配 TURN 时会回空 urls —— 直接当没有，别塞一套废配置给 WebRTC
    if (!data.urls?.length || !data.username || !data.credential) return null;
    return {
      urls: data.urls,
      username: data.username,
      credential: data.credential,
    };
  } catch {
    return null;
  }
}

// ---------- 错误文案 ----------
// 原则：说清"发生了什么" + "下一步点哪里"，不要把内部概念（信令/端口/mDNS）
// 当成用户本来就该懂的东西。

function roomMissingMessage(
  rid: string,
  discovered: DiscoveredServer[],
): string {
  if (discovered.length > 0) {
    const labels = discovered.map((s) => s.label).join("、");
    return (
      `房间 ${rid} 不存在。局域网里发现了 ${discovered.length} 台 Vivi（${labels}），` +
      `但都没有这个房间——房号打错了，或者房主刚关了 App。`
    );
  }
  return `房间 ${rid} 不存在。房号打错了，或者房主刚关了 App / 换了房号。`;
}

async function noDiscoveryMessage(err: string | null): Promise<string> {
  if (err) return `局域网自动发现用不了（${err}）。${DISCOVERY_HINT}`;
  // 本机信令没起来时，"没发现房主"是个误导性的说法，单独说清楚
  try {
    const status = await ipc.getSignalingStatus();
    if (status.port === null) {
      return (
        "本机信令没起来（8080–8089 都被别的程序占用了），" +
        "也没在局域网里发现房主。关掉占用端口的程序后重开，或让房主发邀请给你。"
      );
    }
  } catch {
    /* 拿不到状态就退回通用文案 */
  }
  return `同一个 Wi-Fi 下没发现房主。${DISCOVERY_HINT}`;
}

function manualUnreachableMessage(base: string): string {
  return (
    `连不上信令 ${base}。常见原因：房主那台 App 关了、两台不在同一个网络、` +
    "或者跨网络时房主没做过端口映射。要改地址去「高级 → 连接设置」。"
  );
}

async function localSignalingDownMessage(
  base: string,
  cause: unknown,
): Promise<string> {
  try {
    const status = await ipc.getSignalingStatus();
    if (status.port === null) {
      return "建房失败：本机信令没起来（8080–8089 都被别的程序占用了）。关掉占用端口的程序后重开。";
    }
  } catch {
    /* 拿不到状态就用原始原因 */
  }
  return `建房失败：连不上本机信令 ${base}（${String(cause)}）。`;
}
