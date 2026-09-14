import { useVoiceStore } from "../stores/useVoiceStore";
import { levelIsHot, useMicLevel } from "../hooks/useMicLevel";
import { GearIcon, HeadphoneIcon, LeaveIcon, MicIcon } from "./icons";

export function ControlsBar({ onLeave }: { onLeave: () => void }) {
  const muted = useVoiceStore((s) => s.muted);
  const deafened = useVoiceStore((s) => s.deafened);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const setDeafened = useVoiceStore((s) => s.setDeafened);
  const showSettings = useVoiceStore((s) => s.showSettings);
  const setShowSettings = useVoiceStore((s) => s.setShowSettings);
  const pttEnabled = useVoiceStore((s) => s.pttEnabled);
  const pttKey = useVoiceStore((s) => s.pttKey);
  const pttHeld = useVoiceStore((s) => s.pttHeld);
  const speakingSelf = useVoiceStore((s) => s.speakingSelf);
  const { level } = useMicLevel();
  const hot = !muted && (speakingSelf || levelIsHot(level) || pttHeld);

  return (
    <footer className="deck">
      <button
        type="button"
        className={[
          "btn iconbtn",
          muted && !pttEnabled ? "btn-danger" : "",
          hot ? "lit" : "",
        ]
          .filter(Boolean)
          .join(" ")}
        onClick={() => {
          if (pttEnabled) return;
          setMuted(!muted);
        }}
        aria-pressed={muted}
        aria-disabled={pttEnabled || undefined}
        title={pttEnabled ? `按键说话：按住 ${pttKey}` : muted ? "取消静音" : "静音"}
      >
        <MicIcon muted={muted} lit={hot} level={hot ? level : 0} size={18} />
        {pttHeld ? "说话中" : hot ? "说话中" : muted ? "已静音" : "静音"}
      </button>
      <button
        type="button"
        className={deafened ? "btn iconbtn btn-danger" : "btn iconbtn"}
        onClick={() => setDeafened(!deafened)}
        aria-pressed={deafened}
        title={deafened ? "取消闭音" : "闭音：听不见别人"}
      >
        <HeadphoneIcon off={deafened} />
        {deafened ? "已闭音" : "闭音"}
      </button>
      <button
        type="button"
        className={showSettings ? "btn iconbtn lit" : "btn iconbtn"}
        onClick={() => setShowSettings(!showSettings)}
        aria-pressed={showSettings}
        title="设置"
      >
        <GearIcon />
        设置
      </button>
      <button type="button" className="btn iconbtn" onClick={onLeave} title="离开房间">
        <LeaveIcon />
        离开
      </button>
      <span className="me" aria-live="polite">
        <MicIcon lit={hot} muted={muted} level={hot ? level : 0} size={16} />
        {pttHeld || hot
          ? "你在说话"
          : pttEnabled
            ? `按住 ${pttKey} 说话`
            : muted
              ? "已静音"
              : "静默"}
      </span>
    </footer>
  );
}
