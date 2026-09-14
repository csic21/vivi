/** Tauri IPC 类型化封装（命令见 src-tauri/src/main.rs）。 */
import { invoke } from "@tauri-apps/api/core";
import type { DeviceInfo, PttState, SessionStats, TurnConfig } from "./types";

export const ipc = {
  listAudioDevices: () => invoke<DeviceInfo[]>("list_audio_devices"),
  defaultAudioDevices: () =>
    invoke<[string | null, string | null]>("default_audio_devices"),
  joinRoom: (args: {
    roomId: string;
    input: string | null;
    output: string | null;
    turn: TurnConfig | null;
  }) => invoke<number>("join_room", args),
  leaveRoom: () => invoke<void>("leave_room"),
  setMuted: (muted: boolean) => invoke<void>("set_muted", { muted }),
  setDeafened: (deafened: boolean) => invoke<void>("set_deafened", { deafened }),
  setUserGain: (user: number, gain: number) =>
    invoke<void>("set_user_gain", { user, gain }),
  setMicGain: (gain: number) => invoke<void>("set_mic_gain", { gain }),
  setSpeakerGain: (gain: number) => invoke<void>("set_speaker_gain", { gain }),
  setNsEnabled: (enabled: boolean) => invoke<void>("set_ns_enabled", { enabled }),
  setAgcEnabled: (enabled: boolean) => invoke<void>("set_agc_enabled", { enabled }),
  micLoopbackStart: (args: { input: string | null; output: string | null }) =>
    invoke<void>("mic_loopback_start", args),
  micLoopbackStop: () => invoke<void>("mic_loopback_stop"),
  getStats: () => invoke<SessionStats | null>("get_stats"),
  /** 本端 mic 实时电平 0..1（50ms 轮询，驱动律动条）。 */
  micLevel: () => invoke<number>("mic_level"),
  getPtt: () => invoke<PttState>("get_ptt"),
  setPttEnabled: (enabled: boolean) =>
    invoke<void>("set_ptt_enabled", { enabled }),
  setPttKey: (key: string) => invoke<string>("set_ptt_key", { key }),
};
