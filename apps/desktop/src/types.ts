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
  muted: boolean;
  /** 信令层最后一条错误原文（服务端 SignalMessage::Error）；null = 没出过错 */
  signal_error: string | null;
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

// ---------- 信令现状 / 局域网发现 / 邀请（src-tauri/src/main.rs 的 command 返回值） ----------

/** 本机信令现状。前端靠它渲染状态条、拼地址、判断能不能自动发现。 */
export interface SignalingStatus {
  /** 本机信令的 HTTP base；null = 没起来（8080–8089 都被无关服务占了） */
  local_http: string | null;
  port: number | null;
  /** true = 我们自己起的；false = 复用了已在跑的 signaling 进程 */
  embedded: boolean;
  /** 局域网里别的机器连不连得上这个端口 */
  lan_reachable: boolean;
  lan_ips: string[];
  primary_lan_ip: string | null;
  /** 正在广播的房间号；null = 没建房 */
  advertising_room: string | null;
}

/** 局域网里发现的一台正在广播的 Vivi。 */
export interface DiscoveredServer {
  fullname: string;
  /** 机器名，用来告诉用户"发现了哪台" */
  label: string;
  /** 候选 IPv4，同网段的排前面 */
  addresses: string[];
  port: number;
  /** TXT 里带的房号。只是提示，**不可信**，房间是否存在要探 HTTP */
  room: string | null;
}

export interface DiscoveryResult {
  servers: DiscoveredServer[];
  /** mDNS 起不来时的原因；据此解释"为什么一个都没发现" */
  error: string | null;
}

/** 生成邀请所需的候选地址。 */
export interface InviteInfo {
  room: string;
  port: number | null;
  lan_ips: string[];
  primary_lan_ip: string | null;
  /** 出口公网 IP（STUN 探的 UDP 映射，与 TCP 端口映射无关） */
  public_ip: string | null;
  /** 出口在运营商大内网，端口映射也救不了 */
  cgnat: boolean;
}
