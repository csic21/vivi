/** Rust 侧类型的 TS 镜像（voice-common / voice-session）。 */

export interface DeviceInfo {
  id: string;
  name: string;
  kind: "Input" | "Output";
  is_default: boolean;
}

export type RouteType = "Direct" | "Relay";

export interface PeerStats {
  user_id: number;
  connected: boolean;
  speaking: boolean;
  relayed_via: number | null;
  route: RouteType | null;
  rtt_ms: number | null;
  jitter_ms: number;
  loss_percent: number;
  gain: number;
  decoded_frames: number;
  plc_frames: number;
}

export interface SessionStats {
  peers: PeerStats[];
  mixed_frames: number;
  speaking_self: boolean;
  mic_level: number;
  mic_gain: number;
  speaker_gain: number;
  ns_enabled: boolean;
  agc_enabled: boolean;
}

export interface MicTestReport {
  record_secs: number;
  peak: number;
  rms_dbfs: number;
  clipped_ratio: number;
  played_secs: number;
}

export interface PttState {
  enabled: boolean;
  key: string;
}

export interface TurnConfig {
  urls: string[];
  username: string;
  credential: string;
}
