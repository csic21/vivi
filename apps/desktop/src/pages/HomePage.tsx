import { useEffect, useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { useAudioDevices } from "../hooks/useAudioDevices";
import type { UpdaterState } from "../hooks/useUpdater";
import {
  createRoom,
  ensureRoomReachable,
  normalizeRoomId,
} from "../api/signaling";
import { looksLikeInvite, parseInvite } from "../api/invite";
import {
  getCreateSignalingHttp,
  setAutoSignalingHttp,
  setSignalingHttp,
  wsFromHttp,
} from "../config";
import { ipc } from "../ipc";
import { MicTest } from "../components/MicTest";
import { SignalingPanel } from "../components/SignalingPanel";

export function HomePage({ onJoin, updater }: { onJoin: (roomId: string) => void; updater?: UpdaterState }) {
  const [input, setInput] = useState("");
  const [inviteHint, setInviteHint] = useState<string | null>(null);
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

  /** 入会：解析该连哪个信令（必要时自动发现）+ 预检房间存在，避免进房空等。 */
  const go = async (id: string) => {
    const rid = normalizeRoomId(id);
    if (!rid || busy) return;
    setError(null);
    useVoiceStore.setState({ busy: true });
    try {
      await ensureRoomReachable(rid);
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
      // 建房永远建在本机信令上；再把当前地址钉到本机，别让上一次自动发现
      // 留下的"别人那台"把接下来的入会带偏。
      const rid = await createRoom();
      setAutoSignalingHttp(getCreateSignalingHttp());
      // 广播出去，同一局域网的人输房号就能找到这台。
      // 失败不阻塞建房——广播只影响"能不能被自动发现"，邀请串那条路照样通——
      // 但**必须让房主看见**：以前这里 `.catch(() => undefined)`，广播挂了房主
      // 这边毫无察觉，队友却怎么也发现不到他，两边看到的是完全矛盾的现象。
      useVoiceStore.setState({ advertiseWarning: null });
      await ipc.startAdvertising(rid).catch((e) => {
        useVoiceStore.setState({
          advertiseWarning:
            `这台机器没能在局域网上广播（${String(e)}）。` +
            "同一个 Wi-Fi 的队友可能自动找不到这个房间，把「邀请队友」里的地址发给他。",
        });
      });
      await go(rid);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    }
  };

  /** 房间号输入框也接受直接粘邀请串：一次填好房号 + 地址。 */
  const onPasteRoom = (text: string) => {
    if (!looksLikeInvite(text)) return false;
    const inv = parseInvite(text);
    if (!inv.room && !inv.http) return false;
    if (inv.http) {
      const http = setSignalingHttp(inv.http);
      void ipc.setSignalingUrl(wsFromHttp(http)).catch(() => undefined);
    }
    if (inv.room) setInput(inv.room);
    setError(null);
    setInviteHint(
      `已从邀请串识别：房间 ${inv.room ?? "（未识别）"}` +
        (inv.http ? `，地址 ${inv.http}` : "，将自动查找房主"),
    );
    setTimeout(() => setInviteHint(null), 4000);
    return true;
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
          onPaste={(e) => {
            // 只拦截"看起来像邀请串"的粘贴；粘普通房号照旧
            if (onPasteRoom(e.clipboardData.getData("text"))) e.preventDefault();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void go(input);
          }}
          placeholder="输入房间号，或粘贴邀请"
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
      {inviteHint ? (
        <p className="notice notice-ok" role="status" aria-live="polite">
          {inviteHint}
        </p>
      ) : null}
      {error ? (
        <div className="home-error">
          <p className="notice notice-error" role="alert" aria-live="polite">
            {error}
          </p>
        </div>
      ) : null}
      <div className="home-body">
        <MicTest />
        <SignalingPanel busy={busy} onError={setError} />
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
