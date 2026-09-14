/** 信令 HTTP：建房 + TURN 凭证（webview 直调，CORS 已放行）。 */
import { SIGNALING_HTTP } from "../config";
import type { TurnConfig } from "../types";

export async function createRoom(): Promise<string> {
  const res = await fetch(`${SIGNALING_HTTP}/rooms`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ owner: null }),
  });
  if (!res.ok) throw new Error(`create room failed: ${res.status}`);
  const data = (await res.json()) as { room_id: string };
  return data.room_id;
}

/** 拿 TURN 临时凭证；失败返回 null（纯 P2P 开黑，不阻塞入会）。 */
export async function fetchTurnCreds(userId: number): Promise<TurnConfig | null> {
  try {
    const res = await fetch(
      `${SIGNALING_HTTP}/turn/credentials?user=${userId}`,
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
