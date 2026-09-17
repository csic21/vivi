import { useEffect, useState } from "react";
import { ipc } from "../ipc";
import { useVoiceStore } from "../stores/useVoiceStore";
import type { ManualCode } from "../types";

type Role = "host" | "guest";

/** 手动连接时占位的"房间号"。
 *
 * `App` 靠 roomId 有没有值来决定渲染首页还是通话页，手动连接也得给个非空值；
 * 但它不是真房号（没有房间、没有信令），界面上展示什么由 store 的 `manualMode`
 * 决定，别把这个值当数据用。 */
export const MANUAL_ROOM_LABEL = "manual";

/** 手动连接：不同网络、又没有信令服务器时，靠两人各粘一次连接码把 P2P 接起来。
 *
 * 为什么要有这条路：房间模式要求两端连**同一个**信令进程，而异地时"那个进程在哪"
 * 根本没法自动解决——两台机器手里只有各自的内网地址，没有任何渠道能告诉对方
 * （NAT 的性质，不是实现问题，见 docs/NETWORK.md 第 3 节）。但两人局真正要交换的
 * 只有两段几百字节的文本，那就让人在微信里搬一次。
 *
 * 代价是每连一次要粘两次码；换来的是**零服务器、零入站端口、零第三方**。
 */
export function ManualPanel({ onConnected }: { onConnected: () => void }) {
  const [open, setOpen] = useState(false);
  const [role, setRole] = useState<Role | null>(null);
  const [mine, setMine] = useState<ManualCode | null>(null);
  const [peer, setPeer] = useState("");
  const [working, setWorking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const natKind = useVoiceStore((s) => s.natKind);
  const natProbing = useVoiceStore((s) => s.natProbing);
  const probeNat = useVoiceStore((s) => s.probeNat);
  const setManualMode = useVoiceStore((s) => s.setManualMode);
  const inputDevice = useVoiceStore((s) => s.inputDevice);
  const outputDevice = useVoiceStore((s) => s.outputDevice);

  // 展开时探一次 NAT。对称型的话下面这条路根本走不通，得在用户折腾之前就说清楚。
  useEffect(() => {
    if (open) void probeNat();
  }, [open, probeNat]);

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      setErr("复制失败，手动选中文本复制吧。");
    }
  };

  /** 房主出第一段码。 */
  const hostStart = async () => {
    setErr(null);
    setWorking(true);
    try {
      setMine(await ipc.manualHostStart({ input: inputDevice, output: outputDevice }));
    } catch (e) {
      setErr(humanize(String(e)));
    } finally {
      setWorking(false);
    }
  };

  /** 队友吃进房主的码，出自己的码。 */
  const guestAccept = async () => {
    setErr(null);
    setWorking(true);
    try {
      setMine(
        await ipc.manualGuestAccept({
          hostCode: peer,
          input: inputDevice,
          output: outputDevice,
        }),
      );
    } catch (e) {
      setErr(humanize(String(e)));
    } finally {
      setWorking(false);
    }
  };

  /** 房主粘回队友的码 —— 这一步之后 ICE 才真的开始跑。 */
  const hostFinish = async () => {
    setErr(null);
    setWorking(true);
    try {
      await ipc.manualHostFinish(peer);
      enter();
    } catch (e) {
      setErr(humanize(String(e)));
      setWorking(false);
    }
  };

  /** 进通话页。必须先置 manualMode，否则 RoomPage 一挂载就会去连房间、把这条链路顶掉。 */
  const enter = () => {
    setManualMode(true);
    onConnected();
  };

  const reset = () => {
    setRole(null);
    setMine(null);
    setPeer("");
    setErr(null);
  };

  const natWarn = natKind && !natKind.punching_may_work;

  return (
    <div className="panel manual-panel">
      <button
        type="button"
        className="btn btn-sm manual-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? "收起" : "不同网络？手动连接（不需要服务器）"}
      </button>

      {open ? (
        <div className="manual-detail">
          <p className="hint">
            你和队友不在同一个网络、又没有服务器时，两台机器互相发现不了。
            这个办法绕开这一点：把一段文本在微信里发一次给对方，语音就直接点对点走。
            <strong>不经过任何服务器，也不开任何入站端口。</strong>
          </p>

          {natProbing ? <p className="hint">正在判断你的网络类型…</p> : null}
          {natKind ? (
            <p className={natWarn ? "hint hint-warn" : "hint"}>
              你的网络：{natKind.detail}
            </p>
          ) : null}
          {natWarn ? (
            <p className="hint hint-warn">
              对称型网络下对方无法预测该往哪个端口打，这条路走不通 —— 需要一台带 TURN
              的公网服务器才行（见 <code>docs/NETWORK.md</code> 第 4 节）。
            </p>
          ) : null}

          {role === null ? (
            <div className="manual-roles">
              <button
                type="button"
                className="btn btn-primary"
                disabled={working}
                onClick={() => {
                  setRole("host");
                  void hostStart();
                }}
              >
                我是房主
              </button>
              <button
                type="button"
                className="btn"
                disabled={working}
                onClick={() => setRole("guest")}
              >
                队友让我连他
              </button>
            </div>
          ) : role === "host" ? (
            // 房主：先出码发出去，再等对方把码送回来。两步顺序和队友那边相反。
            <div className="manual-steps">
              <Step no={1} title="把这段码发给队友" />
              {mine ? (
                <CodeBox mine={mine} copied={copied} onCopy={copy} />
              ) : (
                <p className="hint">{working ? "正在生成…" : "点上面的「我是房主」开始。"}</p>
              )}

              <Step no={2} title="把队友发回来的码粘进来" />
              <PeerInput peer={peer} setPeer={setPeer} />
              <button
                type="button"
                className="btn btn-primary"
                disabled={working || !mine || !peer.trim()}
                onClick={() => void hostFinish()}
              >
                {working ? "连接中…" : "开始通话"}
              </button>
              <button type="button" className="btn btn-sm" onClick={reset}>
                重来
              </button>
            </div>
          ) : (
            // 队友：反过来，先吃房主的码才能出自己的。
            <div className="manual-steps">
              <Step no={1} title="把房主的码粘进来" />
              <PeerInput peer={peer} setPeer={setPeer} />

              {mine ? (
                <>
                  <Step no={2} title="把你的码发回给房主" />
                  <CodeBox mine={mine} copied={copied} onCopy={copy} />
                  <button type="button" className="btn btn-primary" onClick={enter}>
                    已经发给他了，进入通话
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={working || !peer.trim()}
                  onClick={() => void guestAccept()}
                >
                  {working ? "生成中…" : "生成我的连接码"}
                </button>
              )}

              <button type="button" className="btn btn-sm" onClick={reset}>
                重来
              </button>
            </div>
          )}

          {err ? (
            <p className="notice notice-error" role="alert" aria-live="polite">
              {err}
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** 步骤头：把"先干嘛再干嘛"摆明，省得用户对着两个输入框发愣。 */
function Step({ no, title }: { no: number; title: string }) {
  return (
    <div className="manual-step">
      <span className="manual-step-no">{no}</span>
      <span className="manual-step-title">{title}</span>
    </div>
  );
}

/** 连接码展示 + 复制。码近千字符，所以限高滚动，别把面板顶穿。 */
function CodeBox({
  mine,
  copied,
  onCopy,
}: {
  mine: ManualCode;
  copied: boolean;
  onCopy: (code: string) => Promise<void>;
}) {
  return (
    <>
      <div className="invite-row-head">
        <span className="invite-title">你的连接码</span>
        <button type="button" className="btn btn-sm" onClick={() => void onCopy(mine.code)}>
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <code className="manual-code">{mine.code}</code>
      <p className="hint">
        共 {mine.code.length} 个字符，微信直接发得下。
        {mine.candidates ? ` ${mine.candidates}。` : ""}
        {mine.crosses_nat ? "" : " 没有 srflx/relay 候选，跨网络连不上。"}
        {mine.reliable ? "" : " 候选没在超时前收齐，连不上就重来一次。"}
      </p>
    </>
  );
}

/** 粘贴对方连接码的输入框。
 *
 * 用 textarea 而不是单行 input：码近千字符，从聊天框粘过来常带换行，
 * 单行 input 会把换行吃掉或显示得没法看（解码侧本来就会剥掉空白）。 */
function PeerInput({ peer, setPeer }: { peer: string; setPeer: (v: string) => void }) {
  return (
    <textarea
      className="field manual-input"
      rows={3}
      spellCheck={false}
      autoComplete="off"
      aria-label="对方的连接码"
      value={peer}
      onChange={(e) => setPeer(e.target.value)}
      placeholder="VIVI1.…"
    />
  );
}

/** 把手动连接的报错翻译成人话（尽量说"下一步做什么"）。 */
function humanize(raw: string): string {
  if (raw.includes("Vivi 连接码") || raw.includes("连接码")) return raw;
  if (raw.includes("permission") || raw.includes("denied")) {
    return "麦克风权限被拒绝：去「系统设置 → 隐私与安全性 → 麦克风」里允许后重试。";
  }
  if (raw.includes("session ended")) {
    return "会话已经结束了，点「重来」重新开始一次。";
  }
  return raw;
}
