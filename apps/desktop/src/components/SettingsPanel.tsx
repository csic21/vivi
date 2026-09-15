import { useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { useAudioDevices } from "../hooks/useAudioDevices";
import { levelToPct, useMicLevel } from "../hooks/useMicLevel";
import { ipc } from "../ipc";
import { MicTest } from "./MicTest";

function displayKey(key: string): string {
  switch (key) {
    case "Mouse4":
      return "鼠标侧键4";
    case "Mouse5":
      return "鼠标侧键5";
    case "Mouse3":
      return "鼠标中键";
    case "Grave":
      return "`";
    default:
      return key;
  }
}

function eventToBind(code: string): string | null {
  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit")) return code;
  if (code.startsWith("Arrow")) return code.slice(5);
  if (code === "Backquote") return "Grave";
  if (code === "BracketLeft") return "LeftBracket";
  if (code === "BracketRight") return "RightBracket";
  if (code === "Backslash") return "BackSlash";
  if (code === "Quote") return "Apostrophe";
  if (code === "Period") return "Dot";
  if (code === "AltLeft" || code === "AltRight") return "Alt";
  if (code === "ControlLeft" || code === "ControlRight") return "Control";
  if (code === "ShiftLeft" || code === "ShiftRight") return "Shift";
  if (code === "MetaLeft" || code === "MetaRight" || code === "OSLeft" || code === "OSRight") {
    return null;
  }
  return code;
}

/**
 * 设置：设备（房间内切换自动重进）+ 音频（试麦/麦音量/扬声器/降噪/增强）
 * + 输入模式（自由说话 / 按键说话）。按键说话由后端轮询 HID，不注册系统热键。
 */
export function SettingsPanel() {
  const inputDevice = useVoiceStore((s) => s.inputDevice);
  const outputDevice = useVoiceStore((s) => s.outputDevice);
  const setDevices = useVoiceStore((s) => s.setDevices);
  const rejoin = useVoiceStore((s) => s.rejoin);
  const pttEnabled = useVoiceStore((s) => s.pttEnabled);
  const pttKey = useVoiceStore((s) => s.pttKey);
  const setPtt = useVoiceStore((s) => s.setPtt);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const micGain = useVoiceStore((s) => s.micGain);
  const setMicGain = useVoiceStore((s) => s.setMicGain);
  const speakerGain = useVoiceStore((s) => s.speakerGain);
  const setSpeakerGain = useVoiceStore((s) => s.setSpeakerGain);
  const nsEnabled = useVoiceStore((s) => s.nsEnabled);
  const setNsEnabled = useVoiceStore((s) => s.setNsEnabled);
  const agcEnabled = useVoiceStore((s) => s.agcEnabled);
  const setAgcEnabled = useVoiceStore((s) => s.setAgcEnabled);
  const { devices } = useAudioDevices();
  const [capturing, setCapturing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);

  const inputs = devices.filter((d) => d.kind === "Input");
  const outputs = devices.filter((d) => d.kind === "Output");

  const changeDevice = async (input: string | null, output: string | null) => {
    setDevices(input, output);
    setMsg("设备已切换，正在重进房间…");
    try {
      await rejoin();
      setMsg(null);
    } catch (e) {
      setMsg(`重进失败：${String(e)}`);
    }
  };

  const togglePtt = async (enabled: boolean) => {
    if (enabled === pttEnabled) return;
    setBusy(true);
    setMsg(null);
    try {
      await ipc.setPttEnabled(enabled);
      const p = await ipc.getPtt();
      setPtt(p.enabled, p.key);
      setMuted(enabled);
    } catch (e) {
      setMsg(`切换失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const applyKey = async (raw: string) => {
    setBusy(true);
    setMsg(null);
    setCapturing(false);
    try {
      const label = await ipc.setPttKey(raw);
      setPtt(pttEnabled, label);
      setMsg(`按键已设为 ${displayKey(label)}，游戏全屏也能用，不会抢走游戏里的这个键。`);
    } catch (e) {
      setMsg(`按键无效：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const { level: fastLevel, stale: meterStale } = useMicLevel();
  const meterPct = levelToPct(fastLevel);

  return (
    <section className="settings" aria-label="设置">
      <h3>设置</h3>
      <div className="row">
        <label htmlFor="set-mic">麦克风</label>
        <select
          id="set-mic"
          value={inputDevice ?? ""}
          onChange={(e) => void changeDevice(e.target.value || null, outputDevice)}
        >
          <option value="">默认设备</option>
          {inputs.map((d) => (
            <option key={d.id} value={d.name}>
              {d.name}
            </option>
          ))}
        </select>
        <label htmlFor="set-spk">扬声器</label>
        <select
          id="set-spk"
          value={outputDevice ?? ""}
          onChange={(e) => void changeDevice(inputDevice, e.target.value || null)}
        >
          <option value="">默认设备</option>
          {outputs.map((d) => (
            <option key={d.id} value={d.name}>
              {d.name}
            </option>
          ))}
        </select>
      </div>
      <MicTest compact />
      <div className="row">
        <label htmlFor="mic-gain">麦克风音量 {Math.round(micGain * 100)}%</label>
        <input
          id="mic-gain"
          type="range"
          min={0}
          max={200}
          value={Math.round(micGain * 100)}
          onChange={(e) => setMicGain(Number(e.target.value) / 100)}
          style={{ flex: 1, minWidth: 120 }}
        />
        <span
          className="levelbar"
          role="meter"
          aria-label={`麦克风电平 ${meterPct}%`}
          aria-valuenow={meterPct}
          aria-valuemin={0}
          aria-valuemax={100}
          title="麦克风实时电平"
          style={{ width: 90 }}
        >
          <span
            className={meterPct > 90 ? "levelfill hot" : "levelfill"}
            style={{ transform: `scaleX(${meterPct / 100})` }}
          />
        </span>
        {meterStale ? (
          <span className="sub" role="status">
            电平无数据：重启应用
          </span>
        ) : null}
      </div>
      <div className="row">
        <label htmlFor="spk-gain">扬声器音量 {Math.round(speakerGain * 100)}%</label>
        <input
          id="spk-gain"
          type="range"
          min={0}
          max={200}
          value={Math.round(speakerGain * 100)}
          onChange={(e) => setSpeakerGain(Number(e.target.value) / 100)}
          style={{ flex: 1, minWidth: 120 }}
        />
      </div>
      <div className="row">
        <label>
          <input
            className="check"
            type="checkbox"
            checked={nsEnabled}
            onChange={(e) => setNsEnabled(e.target.checked)}
          />
          麦克风降噪
        </label>
        <label>
          <input
            className="check"
            type="checkbox"
            checked={agcEnabled}
            onChange={(e) => setAgcEnabled(e.target.checked)}
          />
          麦克风增强（自动增益）
        </label>
      </div>
      <div className="row">
        <span id="talk-mode-label">输入模式</span>
        <div className="modes" role="radiogroup" aria-labelledby="talk-mode-label">
          <button
            type="button"
            role="radio"
            className={pttEnabled ? "mode" : "mode on"}
            aria-checked={!pttEnabled}
            disabled={busy}
            onClick={() => void togglePtt(false)}
          >
            自由说话
          </button>
          <button
            type="button"
            role="radio"
            className={pttEnabled ? "mode on" : "mode"}
            aria-checked={pttEnabled}
            disabled={busy}
            onClick={() => void togglePtt(true)}
          >
            按键说话
          </button>
        </div>
      </div>
      {pttEnabled ? (
        <div className="row">
          <label htmlFor="ptt-key">按键</label>
          <button
            id="ptt-key"
            type="button"
            className={capturing ? "btn bindkey capturing" : "btn bindkey"}
            disabled={busy}
            title="点一下，再按下要绑定的键"
            onClick={() => setCapturing(true)}
            onBlur={() => setCapturing(false)}
            onKeyDown={(e) => {
              e.preventDefault();
              e.stopPropagation();
              if (e.repeat) return;
              if (e.code === "Escape") {
                setCapturing(false);
                return;
              }
              const bind = eventToBind(e.code);
              if (bind) void applyKey(bind);
            }}
            onMouseDown={(e) => {
              if (!capturing || e.button < 3) return;
              e.preventDefault();
              void applyKey(`Mouse${e.button + 1}`);
            }}
          >
            {capturing ? "按下要绑定的键…" : displayKey(pttKey)}
          </button>
        </div>
      ) : null}
      <p className="hint">
        {pttEnabled
          ? `按住 ${displayKey(pttKey)} 开麦，松开静音。全局生效，不会抢走游戏里的这个键。macOS 需在「辅助功能」里允许本应用。`
          : "自由说话：开麦后一直能说话，点底部「静音」可关麦。"}
      </p>
      {msg ? (
        <p className="hint" role="status" aria-live="polite">
          {msg}
        </p>
      ) : null}
    </section>
  );
}
