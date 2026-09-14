import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface AudioDevice {
  id: string;
  name: string;
  kind: "Input" | "Output";
  is_default: boolean;
}

/// 经 Tauri IPC 从 voice-core 拉设备列表（React 不碰实时音频）。
export function useAudioDevices() {
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<AudioDevice[]>("list_audio_devices")
      .then(setDevices)
      .catch((e) => setError(String(e)));
  }, []);

  return { devices, error };
}
