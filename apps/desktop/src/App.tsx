import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { HomePage } from "./pages/HomePage";
import { RoomPage } from "./pages/RoomPage";
import { useUpdater } from "./hooks/useUpdater";
import { UpdaterBanner } from "./components/UpdaterBanner";
import { useVoiceStore } from "./stores/useVoiceStore";

export function App() {
  const [roomId, setRoomId] = useState<string | null>(null);
  const updater = useUpdater();

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
    <>
      <UpdaterBanner updater={updater} />
      {roomId ? (
        <RoomPage roomId={roomId} onLeave={() => setRoomId(null)} />
      ) : (
        <HomePage onJoin={setRoomId} />
      )}
    </>
  );
}
