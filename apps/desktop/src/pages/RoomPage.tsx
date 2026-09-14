import { useEffect, useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { ipc } from "../ipc";
import { MemberList } from "../components/MemberList";
import { ControlsBar } from "../components/ControlsBar";
import { StatsBar } from "../components/StatsBar";
import { SettingsPanel } from "../components/SettingsPanel";
import { CopyIcon } from "../components/icons";

export function RoomPage({
  roomId,
  onLeave,
}: {
  roomId: string;
  onLeave: () => void;
}) {
  const joinCurrentRoom = useVoiceStore((s) => s.joinCurrentRoom);
  const leaveRoom = useVoiceStore((s) => s.leaveRoom);
  const applyStats = useVoiceStore((s) => s.applyStats);
  const error = useVoiceStore((s) => s.error);
  const showSettings = useVoiceStore((s) => s.showSettings);
  const userId = useVoiceStore((s) => s.userId);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let alive = true;
    void joinCurrentRoom()
      .then(() => {
        // 入会后把记住的音频偏好推给新会话（后端会话是全新的，默认全开）
        const st = useVoiceStore.getState();
        void ipc.setMicGain(st.micGain);
        void ipc.setSpeakerGain(st.speakerGain);
        void ipc.setNsEnabled(st.nsEnabled);
        void ipc.setAgcEnabled(st.agcEnabled);
      })
      .catch(() => undefined);
    const timer = setInterval(() => {
      if (alive) applyStats();
    }, 500);
    return () => {
      alive = false;
      clearInterval(timer);
      void leaveRoom();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [roomId]);

  const quit = async () => {
    await leaveRoom();
    onLeave();
  };

  const copyRoom = async () => {
    try {
      await navigator.clipboard.writeText(roomId);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* 剪贴板不可用时忽略，用户手动复制 */
    }
  };

  return (
    <main className="shell">
      <header className="roomhead">
        <div>
          <h2>
            <span className="roomid">{roomId}</span>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => void copyRoom()}
              title="复制房间号发给队友"
            >
              <CopyIcon />
              {copied ? "已复制" : "复制"}
            </button>
          </h2>
          <span className="self">你在 {userId ?? "连接中"} 号位</span>
        </div>
        <StatsBar />
      </header>
      {error ? (
        <p className="notice notice-error" role="alert" aria-live="polite">
          {error}
        </p>
      ) : null}
      <MemberList />
      <ControlsBar onLeave={() => void quit()} />
      {showSettings ? <SettingsPanel /> : null}
    </main>
  );
}
