import { useVoiceStore } from "../stores/useVoiceStore";

export function ControlsBar({ onLeave }: { onLeave: () => void }) {
  const muted = useVoiceStore((s) => s.muted);
  const deafened = useVoiceStore((s) => s.deafened);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const setDeafened = useVoiceStore((s) => s.setDeafened);
  const showSettings = useVoiceStore((s) => s.showSettings);
  const setShowSettings = useVoiceStore((s) => s.setShowSettings);
  const pttEnabled = useVoiceStore((s) => s.pttEnabled);
  const pttKey = useVoiceStore((s) => s.pttKey);
  const speakingSelf = useVoiceStore((s) => s.speakingSelf);

  return (
    <footer className="deck">
      <button
        className={muted && !pttEnabled ? "btn btn-danger" : "btn"}
        onClick={() => setMuted(!muted)}
        disabled={pttEnabled}
        title={pttEnabled ? `按键说话模式：按住 ${pttKey}` : "静音开关"}
      >
        {muted ? "取消静音" : "静音"}
      </button>
      <button className="btn" onClick={() => setDeafened(!deafened)}>
        {deafened ? "取消 deaf" : "Deafen"}
      </button>
      <button className="btn" onClick={() => setShowSettings(!showSettings)}>
        设置
      </button>
      <button className="btn" onClick={onLeave}>
        离开
      </button>
      <span className="me">
        <span className={speakingSelf ? "lamp on" : "lamp"} aria-hidden="true" />
        {speakingSelf ? "你在说话" : pttEnabled ? `按住 ${pttKey} 说话` : "静默"}
      </span>
    </footer>
  );
}
