/** 邀请串：跨网络（mDNS 到不了）时把"房号 + 房主地址"一次性递给对方。
 *
 * 格式：`vivi://<房号>@<host>:<port>`
 *   - `vivi://a1b2c3d4`                    同一局域网，靠自动发现找房主（优先给这个）
 *   - `vivi://a1b2c3d4@192.168.1.10:8080`  显式地址（mDNS 不通 / 异地的局域网地址）
 *   - `vivi://a1b2c3d4@1.2.3.4:8080`       异地（前提是房主做过端口映射）
 *
 * 为什么用这个形状：它同时是合法 URL（`userinfo@host:port`），将来注册 `vivi://`
 * 协议处理器时 URL 解析能直接用，不用自己写解析器。现在只做"粘进输入框"。
 *
 * 不编码 base64/JSON：多一层不可读、不可手改的间接层，出问题时用户和我们都看不懂。
 */

export interface Invite {
  /** 8 位十六进制房号 */
  room: string | null;
  /** 信令 HTTP base，形如 `http://192.168.1.10:8080` */
  http: string | null;
}

/** 房号：建房产出的是 8 位小写 hex，手输常带空格/大写。 */
const ROOM_RE = /[0-9a-f]{8}/i;
/** 显式地址：URL 或裸 host:port。中英文标点都当分隔符。 */
const SEPARATORS = "\\s，,、;；\"'<>（）()【】\\[\\]";
const VIVI_RE = new RegExp(
  `vivi://([0-9a-f]{8})(?:@([^${SEPARATORS}/]+))?`,
  "i",
);
const HTTP_RE = new RegExp(`https?://[^${SEPARATORS}"'<>]+`, "i");

export function formatInvite(room: string, host?: string, port?: number): string {
  const rid = room.trim().toLowerCase();
  if (!host) return `vivi://${rid}`;
  return `vivi://${rid}@${host}${port ? `:${port}` : ""}`;
}

/** 纯文本兜底：聊天软件里 `vivi://` 可能被吞掉，也给一行能手抄的。 */
export function formatInvitePlain(room: string, host?: string, port?: number): string {
  const rid = room.trim().toLowerCase();
  if (!host) return `房间 ${rid}（同一个 Wi-Fi 直接输房号就行）`;
  return `房间 ${rid}，信令地址 http://${host}${port ? `:${port}` : ""}`;
}

/**
 * 从用户粘贴的内容里尽量解读出房号和地址。
 *
 * 故意宽容——用户会粘整段邀请、粘一半、粘别人转发的纯文本。
 * 认不出来就返回 `{ room: null, http: null }`，由调用方决定要不要报错。
 */
export function parseInvite(text: string): Invite {
  const raw = text.trim();
  if (!raw) return { room: null, http: null };

  const vivi = VIVI_RE.exec(raw);
  if (vivi) {
    return {
      room: vivi[1].toLowerCase(),
      http: vivi[2] ? `http://${vivi[2]}` : null,
    };
  }

  // 纯文本 / 网址混合：分别找地址和房号，找到哪个算哪个
  const httpMatch = HTTP_RE.exec(raw);
  const roomMatch = ROOM_RE.exec(raw);
  let http = httpMatch?.[0] ?? null;
  if (http) {
    // 去掉路径和查询串，只留 origin（`http://host:port/#room` 这类）
    http = http.replace(/\/(?=[^/]*$).*$/, "").replace(/\/+$/, "");
    try {
      const u = new URL(http);
      http = `${u.protocol}//${u.host}`;
    } catch {
      http = null;
    }
  }

  return { room: roomMatch ? roomMatch[0].toLowerCase() : null, http };
}

/** 用户粘进来的是不是"看起来像邀请"（用来决定要不要覆盖已有输入）。 */
export function looksLikeInvite(text: string): boolean {
  const t = text.trim();
  return t.startsWith("vivi:") || /^https?:\/\//i.test(t);
}
