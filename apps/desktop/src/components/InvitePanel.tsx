import { useEffect, useState } from "react";
import { formatInvite } from "../api/invite";
import { ipc } from "../ipc";
import type { InviteInfo } from "../types";

/** 邀请面板：把"房号 + 房主地址"变成一段可复制的短串。
 *
 * 同一局域网其实**不需要**这个（mDNS 会自动找到房主），所以主推的永远是"直接发房号"。
 * 只有跨网络——没有信令服务器时，两台机器之间没有任何途径能互相发现——才需要
 * 用房主地址。这是产品上的取舍：不假装异地也能自动，但要把手填成本压到一次复制粘贴。
 */
export function InvitePanel({ roomId }: { roomId: string }) {
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState<InviteInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    if (!open || info !== null) return;
    let cancelled = false;
    void ipc
      .buildInvite(roomId)
      .then((v) => {
        if (!cancelled) setInfo(v);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, info, roomId]);

  const copy = async (text: string, tag: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(tag);
      setTimeout(() => setCopied(null), 1500);
    } catch {
      setError("复制失败，手动选中文本复制吧。");
    }
  };

  const port = info?.port ?? null;

  const rows: Array<{
    tag: string;
    title: string;
    text: string;
    note?: string;
    warn?: boolean;
  }> = [];
  if (port !== null) {
    rows.push({
      tag: "lan-auto",
      title: "同一个 Wi-Fi · 直接把房号发给对方",
      text: roomId,
      note: "对方在首页输这个房号就行，不用填地址。",
    });
    if (info?.primary_lan_ip) {
      rows.push({
        tag: "lan-manual",
        title: "同一个 Wi-Fi 但自动发现被挡住（公司网 / 访客网络）",
        text: formatInvite(roomId, info.primary_lan_ip, port),
        note: "对方粘进房间号框即可。",
      });
    }
    if (info?.public_ip) {
      rows.push({
        tag: "wan",
        title: "不同网络（异地）",
        text: formatInvite(roomId, info.public_ip, port),
        note:
          "前提是你在路由器上把 " +
          `${port}/tcp 端口映射到了这台机器。没做过映射的话对方会连不上——` +
          "那就只能等以后加一台公网信令服务器，或者让对方连你同一个 Wi-Fi。",
      });
    }
  }

  return (
    <div className="panel invite-panel">
      <button
        type="button"
        className="btn btn-sm invite-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? "收起邀请" : "邀请队友"}
      </button>

      {open ? (
        <div className="invite-detail">
          {port === null ? (
            <p className="hint hint-warn">
              本机信令没起来（8080–8089 都被别的程序占用了），局域网里发现不到这台，
              也没法生成邀请。关掉占用端口的程序后重开试试。
            </p>
          ) : null}

          {info?.cgnat ? (
            <p className="hint hint-warn">
              你的宽带走的是运营商大内网（CGNAT），没有自己的公网 IP，端口映射也做不了。
              异地这条路走不通——让对方连你同一个 Wi-Fi，或者以后加一台公网信令服务器。
            </p>
          ) : null}

          {error ? <p className="hint hint-warn">{error}</p> : null}

          {rows.map((row) => (
            <div className="invite-row" key={row.tag}>
              <div className="invite-row-head">
                <span className="invite-title">{row.title}</span>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => void copy(row.text, row.tag)}
                >
                  {copied === row.tag ? "已复制" : "复制"}
                </button>
              </div>
              <code className="invite-code">{row.text}</code>
              {row.note ? <p className="hint">{row.note}</p> : null}
            </div>
          ))}

          <p className="hint">
            信令通了只代表能交换成员名单，声音是另一条 P2P 链路：双方都在对称 NAT 后
            且没有中继时，可能"连上了但没声音"。
          </p>
        </div>
      ) : null}
    </div>
  );
}
