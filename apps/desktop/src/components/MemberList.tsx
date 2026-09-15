import type { CSSProperties } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { ipc } from "../ipc";
import { levelIsHot, levelToPct, useMicLevel } from "../hooks/useMicLevel";
import { MicIcon } from "./icons";

export function MemberList() {
  const members = useVoiceStore((s) => s.members);
  const userId = useVoiceStore((s) => s.userId);
  const speakingSelf = useVoiceStore((s) => s.speakingSelf);
  const muted = useVoiceStore((s) => s.muted);
  const pttEnabled = useVoiceStore((s) => s.pttEnabled);
  const pttKey = useVoiceStore((s) => s.pttKey);
  const pttHeld = useVoiceStore((s) => s.pttHeld);
  const { level, stale } = useMicLevel();
  const pct = levelToPct(level);
  const selfHot = !muted && (speakingSelf || levelIsHot(level) || pttHeld);

  return (
    <>
      <div className={selfHot ? "member self speaking" : "member self"}>
        <span className="rail" aria-hidden="true" />
        <span
          className={selfHot ? "avatar lit" : "avatar"}
          style={{ ["--lvl"]: String(pct / 100) } as CSSProperties}
        >
          <MicIcon lit={selfHot} muted={muted} level={selfHot ? level : 0} size={18} />
        </span>
        <span className="who">
          <span className="name">你{userId != null ? ` · ${userId} 号位` : ""}</span>
          <br />
          <span className="sub">
            {stale
              ? "电平无数据：重启应用"
              : selfHot
                ? "正在说话"
                : muted
                  ? pttEnabled
                    ? `按住 ${pttKey} 说话`
                    : "已静音"
                  : "本端麦克风"}
          </span>
        </span>
        <span
          className="levelbar"
          role="meter"
          aria-label="麦克风电平"
          aria-valuenow={pct}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <span
            className={pct > 90 ? "levelfill hot" : "levelfill"}
            style={{ transform: `scaleX(${pct / 100})` }}
          />
        </span>
      </div>
      {members.length === 0 ? (
        <p className="squad-empty">暂无队友。把房间号发给队友，对方进来后会自动出现在这里。</p>
      ) : (
        <>
          <p className="caption">
            队友 · {members.length}
          </p>
          <ul className="squad" aria-live="polite">
          {[...members]
            .sort((a, b) => a.user_id - b.user_id)
            .map((m) => (
              <li key={m.user_id} className={m.speaking ? "member speaking" : "member"}>
                <span className="rail" aria-hidden="true" />
                <span className={m.speaking ? "avatar lit" : "avatar"}>
                  <MicIcon lit={m.speaking} size={18} />
                </span>
                <span className="who">
                  <span className="name">{m.user_id} 号位</span>
                  <br />
                  <span className="sub">
                    {!m.connected
                      ? "连接中"
                      : m.speaking
                        ? "正在说话"
                        : m.relayed_via != null
                          ? `经 ${m.relayed_via} 号位中转 · ${Math.round(m.gain * 100)}%`
                          : `${m.rtt_ms ?? "—"}ms · ${m.loss_percent.toFixed(1)}% · ${
                              m.route === "Relay" ? "中继" : "直连"
                            } · ${Math.round(m.gain * 100)}%`}
                  </span>
                </span>
                <input
                  type="range"
                  min={0}
                  max={200}
                  value={Math.round(m.gain * 100)}
                  aria-label={`${m.user_id} 号位音量`}
                  onChange={(e) => {
                    const gain = Number(e.target.value) / 100;
                    useVoiceStore.setState((st) => ({
                      members: st.members.map((x) =>
                        x.user_id === m.user_id ? { ...x, gain } : x,
                      ),
                    }));
                    void ipc.setUserGain(m.user_id, gain);
                  }}
                />
              </li>
            ))}
          </ul>
        </>
      )}
    </>
  );
}
