import { useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { useAudioDevices } from "../hooks/useAudioDevices";
import { levelToPct, useMicLevel } from "../hooks/useMicLevel";
import { ipc } from "../ipc";

/**
 * 设置：设备（房间内切换自动重进）+ 音频（试麦/麦音量/扬声器/降噪/增强）
 * + 按键说话（系统级热键，后端注册，只支持键盘）。
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
  const [keyDraft, setKeyDraft] = useState(pttKey);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [listening, setListening] = useState(false);

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
    setBusy(true);
    setMsg(null);
    try {
      await ipc.setPttEnabled(enabled);
      const p = await ipc.getPtt();
      setPtt(p.enabled, p.key);
      if (enabled) setMuted(true);
    } catch (e) {
      setMsg(`切换失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const applyKey = async () => {
    setBusy(true);
    setMsg(null);
    try {
      await ipc.setPttKey(keyDraft);
      const p = await ipc.getPtt();
      setPtt(p.enabled, p.key);
      setMsg(`按键已设为 ${p.key}，全局生效，游戏全屏也能用。`);
    } catch (e) {
      setMsg(`按键无效：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const toggleListen = async () => {
    if (listening) {
      await ipc.micLoopbackStop().catch(() => undefined);
      setListening(false);
      return;
    }
    setMsg(null);
    try {
      await ipc.micLoopbackStart({ input: inputDevice, output: outputDevice });
      setListening(true);
    } catch (e) {
      setMsg(`试听失败：${String(e)}`);
    }
  };

  // 米表：用 50ms 快通道（store 里的是 500ms 快照，太顿）
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
        <button
          className={listening ? "btn btn-danger" : "btn"}
          onClick={toggleListen}
          title="实时从扬声器听到自己的麦克风（含降噪增益链路）"
        >
          {listening ? "停止试听" : "试听麦克风"}
        </button>
      </div>
      {listening && (
        <p className="hint" role="status">
          正在实时试听…请戴耳机，否则会啸叫。进房间会自动停止。
        </p>
      )}
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
        {meterStale && (
          <span className="sub" role="status">
            电平无数据：重启 tauri dev
          </span>
        )}
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
        <label>
          <input
            className="check"
            type="checkbox"
            checked={pttEnabled}
            disabled={busy}
            onChange={(e) => void togglePtt(e.target.checked)}
          />
          按键说话
        </label>
        <label htmlFor="ptt-key">按键</label>
        <input
          id="ptt-key"
          type="text"
          value={keyDraft}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => setKeyDraft(e.target.value)}
          placeholder="V…"
        />
        <button className="btn" disabled={busy} onClick={applyKey}>
          应用
        </button>
      </div>
      <p className="hint">
        按键说话由后端注册为系统热键，前端卡死不影响收发。只支持键盘按键，不支持鼠标侧键。
      </p>
      {msg && (
        <p className="hint" role="status" aria-live="polite">
          {msg}
        </p>
      )}
    </section>
  );
}
