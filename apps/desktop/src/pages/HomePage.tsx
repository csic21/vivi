import { useEffect, useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { useAudioDevices } from "../hooks/useAudioDevices";
import { createRoom } from "../api/signaling";
import { ipc } from "../ipc";
import { MicTest } from "../components/MicTest";

export function HomePage({ onJoin }: { onJoin: (roomId: string) => void }) {
  const [input, setInput] = useState("");
  const setRoomInput = useVoiceStore((s) => s.setRoomInput);
  const busy = useVoiceStore((s) => s.busy);
  const error = useVoiceStore((s) => s.error);
  const setError = useVoiceStore((s) => s.setError);
  const setDevices = useVoiceStore((s) => s.setDevices);
  const inputDevice = useVoiceStore((s) => s.inputDevice);
  const outputDevice = useVoiceStore((s) => s.outputDevice);
  const { devices } = useAudioDevices();

  const inputs = devices.filter((d) => d.kind === "Input");
  const outputs = devices.filter((d) => d.kind === "Output");

  useEffect(() => {
    if (inputDevice === null) {
      const def = inputs.find((d) => d.is_default);
      if (def) setDevices(def.name, outputDevice);
    }
    if (outputDevice === null) {
      const def = outputs.find((d) => d.is_default);
      if (def) setDevices(inputDevice, def.name);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [devices]);

  useEffect(() => {
    // 启动时把记住的偏好推给后端（PTT 监听随进程，必须重建）
    void ipc.getPtt().then((p) => {
      const st = useVoiceStore.getState();
      const key = st.pttKey || p.key;
      void ipc
        .setPttKey(key)
        .then((label) => {
          if (st.pttEnabled) {
            return ipc.setPttEnabled(true).then(() => st.setPtt(true, label));
          }
          st.setPtt(false, label);
        })
        .catch(() => st.setPtt(p.enabled, p.key));
    });
  }, []);

  const go = (id: string) => {
    const rid = id.trim();
    if (!rid) return;
    setError(null);
    setRoomInput(rid);
    onJoin(rid);
  };

  const create = async () => {
    setError(null);
    try {
      go(await createRoom());
    } catch (e) {
      setError(`建房失败，先确认信令在跑：cargo run -p signaling。${String(e)}`);
    }
  };

  return (
    <main className="shell">
      <div className="brand">
        <h1>
          GameVoice<span className="dot">.</span>
        </h1>
        <p>小队语音，快、轻、稳定。先试麦，再进房。</p>
      </div>
      <div className="joinrow">
        <input
          className="field"
          name="room"
          aria-label="房间 ID"
          autoComplete="off"
          spellCheck={false}
          autoFocus
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") go(input);
          }}
          placeholder="输入房间号"
        />
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !input.trim()}
          onClick={() => go(input)}
        >
          {busy ? "加入中…" : "加入"}
        </button>
        <button type="button" className="btn" disabled={busy} onClick={() => void create()}>
          建房
        </button>
      </div>
      <fieldset className="devices">
        <legend>音频设备</legend>
        <div className="device-row">
          <label htmlFor="mic">麦克风</label>
          <select
            id="mic"
            value={inputDevice ?? ""}
            onChange={(e) => setDevices(e.target.value || null, outputDevice)}
          >
            <option value="">默认设备</option>
            {inputs.map((d) => (
              <option key={d.id} value={d.name}>
                {d.name}
              </option>
            ))}
          </select>
        </div>
        <div className="device-row">
          <label htmlFor="spk">扬声器</label>
          <select
            id="spk"
            value={outputDevice ?? ""}
            onChange={(e) => setDevices(inputDevice, e.target.value || null)}
          >
            <option value="">默认设备</option>
            {outputs.map((d) => (
              <option key={d.id} value={d.name}>
                {d.name}
              </option>
            ))}
          </select>
        </div>
      </fieldset>
      <MicTest />
      {error ? (
        <p className="notice notice-error" role="alert" aria-live="polite">
          {error}
        </p>
      ) : null}
    </main>
  );
}
