import { useVoiceStore } from "../stores/useVoiceStore";
import { ipc } from "../ipc";
import { levelToPct, useMicLevel } from "../hooks/useMicLevel";

export function MemberList() {
  const members = useVoiceStore((s) => s.members);
  const userId = useVoiceStore((s) => s.userId);
  const speakingSelf = useVoiceStore((s) => s.speakingSelf);
  const { level, stale } = useMicLevel();
  const pct = levelToPct(level);
  return (
    <>
      <div className={speakingSelf ? "member self speaking" : "member self"}>
        <span className="rail" aria-hidden="true" />
        <span className="who">
          <span className="name">你{userId != null ? ` · ${userId} 号位` : ""}</span>
          <br />
          <span className="sub">
            {stale ? "电平无数据：重启 tauri dev" : "本端麦克风"}
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
        <p className="squad-empty">还没队友，把 Room ID 发给他们。</p>
      ) : (
        <ul className="squad" aria-live="polite">
          {[...members]
            .sort((a, b) => a.user_id - b.user_id)
            .map((m) => (
        <li key={m.user_id} className={m.speaking ? "member speaking" : "member"}>
          <span className="rail" aria-hidden="true" />
          <span className="who">
            <span className="name">
              {m.speaking ? "● " : ""}
              {m.user_id} 号位
            </span>
            <br />
            <span className="sub">
              {!m.connected
                ? "连接中"
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
      )}
    </>
  );
}
