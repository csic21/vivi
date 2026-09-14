import { useState } from "react";
import { HomePage } from "./pages/HomePage";
import { RoomPage } from "./pages/RoomPage";
import { useUpdater } from "./hooks/useUpdater";
import { UpdaterBanner } from "./components/UpdaterBanner";

export function App() {
  const [roomId, setRoomId] = useState<string | null>(null);
  const updater = useUpdater();
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
