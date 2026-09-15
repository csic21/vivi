/** 信令 HTTP：建房 + 查房 + TURN 凭证（webview 直调，CORS 已放行）。 */
import { getSignalingHttp, wsFromHttp } from "../config";
import type { TurnConfig } from "../types";

/** 房号规范化：去空格转小写（建房产出 8 位小写 hex，手动输入常带空格/大写）。 */
export function normalizeRoomId(raw: string): string {
  return raw.trim().toLowerCase();
}

export async function createRoom(): Promise<string> {
  const res = await fetch(`${getSignalingHttp()}/rooms`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ owner: null }),
  });
  if (!res.ok) throw new Error(`create room failed: ${res.status}`);
  const data = (await res.json()) as { room_id: string };
  return normalizeRoomId(data.room_id);
}

/** 入会预检：房间不存在直接抛（避免进房空等“还没队友”）。 */
export async function checkRoomExists(roomId: string): Promise<void> {
  const rid = normalizeRoomId(roomId);
  let res: Response;
  try {
    res = await fetch(`${getSignalingHttp()}/rooms/${encodeURIComponent(rid)}`);
  } catch {
    throw new Error(
      `连不上信令 ${getSignalingHttp()}：两台必须填同一个地址（都指向房主的 http://<房主IP>:8080），且房主 App 开着。`,
    );
  }
  if (res.status === 404) {
    throw new Error(
      `房间 ${rid} 不存在：房号打错，或两台连的不是同一个信令（检查首页信令地址是否都指向房主）。建房的那台不要关。`,
    );
  }
  if (!res.ok) throw new Error(`check room failed: ${res.status}`);
}

/** 当前信令对应的 WS 地址（透传给 Rust join_room，保证 HTTP 与 WS 同源）。 */
export function currentSignalingWs(): string {
  return wsFromHttp(getSignalingHttp());
}

/** 拿 TURN 临时凭证；失败返回 null（纯 P2P 开黑，不阻塞入会）。 */
export async function fetchTurnCreds(userId: number): Promise<TurnConfig | null> {
  try {
    const res = await fetch(
      `${getSignalingHttp()}/turn/credentials?user=${userId}`,
    );
    if (!res.ok) return null;
    const data = (await res.json()) as {
      urls: string[];
      username: string;
      credential: string;
    };
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
