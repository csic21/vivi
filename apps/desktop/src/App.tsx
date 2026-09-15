import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { HomePage } from "./pages/HomePage";
import { RoomPage } from "./pages/RoomPage";
import { useUpdater } from "./hooks/useUpdater";
import { UpdaterBanner } from "./components/UpdaterBanner";
import { useVoiceStore } from "./stores/useVoiceStore";
import { bootstrapLocalSignaling } from "./api/signaling";
import { migrateLegacySignalingHttp } from "./config";

export function App() {
  const [roomId, setRoomId] = useState<string | null>(null);
  const updater = useUpdater();

  useEffect(() => {
    // 本机内嵌信令的实际端口由 Rust 决定（8080 被占会往后让），前端在拿到
    // 状态之前只能用构建期默认值兜底，所以这步要尽早跑。
    migrateLegacySignalingHttp();
    void bootstrapLocalSignaling();
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<boolean>("ptt-held", (e) => {
      useVoiceStore.setState({ muted: !e.payload, pttHeld: e.payload });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return (
    <div className="app">
      <UpdaterBanner updater={updater} />
      {roomId ? (
        <RoomPage roomId={roomId} onLeave={() => setRoomId(null)} updater={updater} />
      ) : (
        <HomePage onJoin={setRoomId} updater={updater} />
      )}
    </div>
  );
}
