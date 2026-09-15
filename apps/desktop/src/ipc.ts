/** Tauri IPC 类型化封装（命令见 src-tauri/src/main.rs）。 */
import { invoke } from "@tauri-apps/api/core";
import type {
  DeviceInfo,
  DiscoveredServer,
  DiscoveryResult,
  InviteInfo,
  PttState,
  SessionStats,
  SignalingStatus,
  TurnConfig,
} from "./types";

export const ipc = {
  listAudioDevices: () => invoke<DeviceInfo[]>("list_audio_devices"),
  defaultAudioDevices: () =>
    invoke<[string | null, string | null]>("default_audio_devices"),
  joinRoom: (args: {
    roomId: string;
    input: string | null;
    output: string | null;
    turn: TurnConfig | null;
    signalingUrl?: string;
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
  getSignalingUrl: () => invoke<string>("get_signaling_url"),
  setSignalingUrl: (url: string) => invoke<string>("set_signaling_url", { url }),
  setPttEnabled: (enabled: boolean) =>
    invoke<void>("set_ptt_enabled", { enabled }),
  setPttKey: (key: string) => invoke<string>("set_ptt_key", { key }),

  // ---------- 自动发现 / 邀请 ----------

  /** 本机信令在哪个端口、起没起来、能不能被局域网访问。 */
  getSignalingStatus: () => invoke<SignalingStatus>("get_signaling_status"),
  /** 浏览局域网里正在广播的 Vivi。返回的是候选，房间是否存在要再探 HTTP。 */
  discoverSignaling: (timeoutMs?: number) =>
    invoke<DiscoveryResult>("discover_signaling", { timeoutMs: timeoutMs ?? null }),
  /** 建房后开始广播，让同一局域网的人能靠房号找到这台。 */
  startAdvertising: (roomId: string) =>
    invoke<void>("start_advertising", { roomId }),
  /** 离房时撤销广播。 */
  stopAdvertising: () => invoke<void>("stop_advertising"),
  /** 取生成邀请所需的候选地址（会做一次 STUN 探测）。 */
  buildInvite: (roomId: string) => invoke<InviteInfo>("build_invite", { roomId }),
};

export type { DiscoveredServer, DiscoveryResult, InviteInfo, SignalingStatus };
