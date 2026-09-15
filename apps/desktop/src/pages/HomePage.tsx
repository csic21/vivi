import { useEffect, useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { useAudioDevices } from "../hooks/useAudioDevices";
import type { UpdaterState } from "../hooks/useUpdater";
import { checkRoomExists, createRoom, normalizeRoomId } from "../api/signaling";
import { getSignalingHttp, setSignalingHttp, wsFromHttp } from "../config";
import { ipc } from "../ipc";
import { MicTest } from "../components/MicTest";

export function HomePage({ onJoin, updater }: { onJoin: (roomId: string) => void; updater?: UpdaterState }) {
  const [input, setInput] = useState("");
  const [signalInput, setSignalInput] = useState(getSignalingHttp);
  const [signalSaved, setSignalSaved] = useState(false);
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** 保存信令地址：前端 HTTP 与 Rust WS 同源切换。 */
  const saveSignaling = async () => {
    const raw = signalInput.trim();
    if (!raw) return;
    if (!/^https?:\/\//.test(raw)) {
      setError("信令地址以 http:// 或 https:// 开头，例如 http://192.168.1.10:8080。");
      return;
    }
    const http = setSignalingHttp(raw);
    setSignalInput(http);
    setError(null);
    try {
      await ipc.setSignalingUrl(wsFromHttp(http));
    } catch {
      /* 非 Tauri 预览环境下忽略，后端仍用默认值 */
    }
    setSignalSaved(true);
    setTimeout(() => setSignalSaved(false), 1500);
  };

  /** 入会：规范化房号 + 预检房间存在才进房，避免进房空等“还没队友”。 */
  const go = async (id: string) => {
    const rid = normalizeRoomId(id);
    if (!rid || busy) return;
    setError(null);
    useVoiceStore.setState({ busy: true });
    try {
      await checkRoomExists(rid);
    } catch (e) {
      useVoiceStore.setState({ busy: false });
      setError(String(e instanceof Error ? e.message : e));
      return;
    }
    useVoiceStore.setState({ busy: false });
    setRoomInput(rid);
    onJoin(rid);
  };

  const create = async () => {
    setError(null);
    try {
      await go(await createRoom());
    } catch (e) {
      setError(`建房失败，先确认信令在跑：cargo run -p signaling。${String(e)}`);
    }
  };

  return (
    <main className="shell">
      <header className="brandbar">
        <h1>
          <img className="brand-mark" src="/icon.png" alt="" width={26} height={26} />
          Vivi<span className="dot">.</span>
        </h1>
        <span className="tag">小队语音</span>
      </header>
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
            if (e.key === "Enter") void go(input);
          }}
          placeholder="输入房间号"
        />
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !input.trim()}
          onClick={() => void go(input)}
        >
          {busy ? "加入中…" : "加入"}
        </button>
        <button type="button" className="btn" disabled={busy} onClick={() => void create()}>
          建房
        </button>
      </div>
      {error ? (
        <div className="home-error">
          <p className="notice notice-error" role="alert" aria-live="polite">
            {error}
          </p>
        </div>
      ) : null}
      <div className="home-body">
        <MicTest />
        <fieldset className="panel">
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
        <fieldset className="panel">
          <legend>信令</legend>
          <div className="device-row">
            <label htmlFor="sig">地址</label>
            <input
              id="sig"
              className="field"
              autoComplete="off"
              spellCheck={false}
              value={signalInput}
              onChange={(e) => setSignalInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveSignaling();
              }}
              placeholder="http://房主IP:8080"
            />
            <button type="button" className="btn" disabled={busy} onClick={() => void saveSignaling()}>
              {signalSaved ? "已保存" : "保存"}
            </button>
          </div>
          <p className="hint">
            跨机开黑时，两台填同一个地址，都指向建房那台。地址不同，各连各的本机，房号一样也碰不上面。
            Windows 与 Mac 互通：两台装同一个版本、连同一个信令地址即可互通；公司/校园网不通时需同 Wi-Fi 或开 TURN。
          </p>
        </fieldset>
      </div>
      <HomeVersionFooter updater={updater} />
    </main>
  );
}

function HomeVersionFooter({ updater }: { updater?: UpdaterState }) {
  if (!updater) return null;
  const { status, currentVersion, availableVersion } = updater;
  const versionText = currentVersion ? `Vivi v${currentVersion}` : "Vivi";
  const updateHint =
    status === "available"
      ? ` · 有新版本 v${availableVersion}，去房间内设置里更新`
      : status === "checking" || status === "downloading"
        ? " · 正在更新…"
        : "";
  return (
    <footer className="home-foot" aria-label="版本信息">
      <span>
        {versionText}
        {updateHint}
      </span>
      {status !== "available" && status !== "checking" && status !== "downloading" ? (
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          onClick={() => void updater.checkForUpdate()}
        >
          检查更新
        </button>
      ) : null}
    </footer>
  );
}
