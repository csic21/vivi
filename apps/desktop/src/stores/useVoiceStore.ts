import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ipc } from "../ipc";
import {
  currentSignalingWs,
  ensureRoomReachable,
  fetchTurnCreds,
  normalizeRoomId,
} from "../api/signaling";
import type { NatInfo, PeerStats } from "../types";

/** 旧版 GameVoice 存的偏好 key，首次启动迁移到 "vivi"，避免重命名后偏好丢失。 */
try {
  if (
    typeof localStorage !== "undefined" &&
    localStorage.getItem("vivi") == null &&
    localStorage.getItem("gamevoice") != null
  ) {
    localStorage.setItem("vivi", localStorage.getItem("gamevoice") as string);
  }
} catch {
  /* localStorage 不可用时忽略 */
}

interface VoiceState {
  roomId: string | null;
  userId: number | null;
  busy: boolean;
  error: string | null;
  /** 信令层最后一条错误原文（服务端 SignalMessage::Error）；由 500ms 快照轮询带回 */
  signalError: string | null;
  /** 建房时没能广播到局域网的原因；`null` = 广播正常或没建房 */
  advertiseWarning: string | null;
  members: PeerStats[];
  speakingSelf: boolean;
  micLevel: number;
  micGain: number;
  speakerGain: number;
  nsEnabled: boolean;
  agcEnabled: boolean;
  muted: boolean;
  deafened: boolean;
  pttEnabled: boolean;
  pttKey: string;
  pttHeld: boolean;
  inputDevice: string | null;
  outputDevice: string | null;
  showSettings: boolean;
  listening: boolean;
  /**
   * 这次会话是「手动连接」起来的（不经过房间和信令服务器）。
   * RoomPage 据此跳过入会 —— 否则它一挂载就会去连房间，把手动链路的会话顶掉。
   */
  manualMode: boolean;
  /** 本机 NAT 判定结果；null = 还没探过 */
  natKind: NatInfo | null;
  natProbing: boolean;

  setManualMode: (v: boolean) => void;
  /** 探一次本机 NAT（有缓存：要联网问 STUN，不该每次开面板都等一轮）。 */
  probeNat: () => Promise<NatInfo | null>;
  setRoomInput: (id: string | null) => void;
  setError: (e: string | null) => void;
  setMuted: (m: boolean) => void;
  setDeafened: (d: boolean) => void;
  setPtt: (enabled: boolean, key: string) => void;
  setDevices: (input: string | null, output: string | null) => void;
  setShowSettings: (v: boolean) => void;
  setMicGain: (g: number) => void;
  setSpeakerGain: (g: number) => void;
  setNsEnabled: (v: boolean) => void;
  setAgcEnabled: (v: boolean) => void;
  applyStats: () => void;
  startMicTest: () => Promise<void>;
  stopMicTest: () => Promise<void>;

  joinCurrentRoom: () => Promise<void>;
  leaveRoom: () => Promise<void>;
  rejoin: () => Promise<void>;
}

async function doJoin(
  get: () => VoiceState,
  set: (p: Partial<VoiceState>) => void,
) {
  const { roomId, inputDevice, outputDevice } = get();
  if (!roomId) return;
  const rid = normalizeRoomId(roomId);
  if (rid !== roomId) set({ roomId: rid });
  // 每次（重）进房都从干净的报错状态开始，别把上一次的残留一直挂在界面上。
  // manualMode 一并清掉：走房间模式就意味着放弃手动连接那条链路。
  set({ busy: true, error: null, signalError: null, manualMode: false });
  try {
    // 预检 + 解析该连哪个信令：先试当前地址，不中再做一次局域网自动发现。
    // 房号错 / 找不到房主都在这步报错，不进房空等。
    await ensureRoomReachable(rid);
    // 先拿 TURN 凭证（失败则纯 P2P，不阻塞）
    const turn = await fetchTurnCreds(Date.now() % 100000);
    const userId = await ipc.joinRoom({
      roomId: rid,
      input: inputDevice,
      output: outputDevice,
      turn,
      signalingUrl: currentSignalingWs(),
    });
    set({
      userId,
      busy: false,
      listening: false,
      muted: get().pttEnabled,
      pttHeld: false,
    });
  } catch (e) {
    set({ busy: false, error: friendlyError(String(e)) });
    throw e;
  }
}

/** 把 Rust 报错翻译成人话（带下一步动作，不只说问题）。 */
function friendlyError(raw: string): string {
  // 预检抛出的中文业务错误直接透出（房间/信令），别被下面的英文关键词误判
  if (raw.includes("房间") || raw.includes("信令")) return raw;
  const msg = raw.toLowerCase();
  if (msg.includes("room not found") || msg.includes("room_id") || msg.includes("room ")) {
    return `房间不存在：房号打错，或两台连的不是同一个信令。${raw}`;
  }
  if (msg.includes("permission") || msg.includes("denied") || msg.includes("not permitted")) {
    return "麦克风权限被拒绝：去“系统设置 → 隐私与安全性 → 麦克风”里允许，然后重进房间。";
  }
  if (msg.includes("no default") || msg.includes("not found") || msg.includes("no device")) {
    return "找不到麦克风：检查设备是否插好，或在首页换一个再进。";
  }
  if (msg.includes("unsupported") && msg.includes("sample format")) {
    return "这个音频设备格式不受支持：去首页/设置里换一个麦克风或扬声器再试。";
  }
  if (
    msg.includes("connection refused") ||
    msg.includes("failed to connect") ||
    msg.includes("websocket") ||
    msg.includes("signal")
  ) {
    // 注意区分两件事，否则用户会误以为"连上了就该有声音"：
    // 信令通了只是能交换成员名单，声音是另一条 P2P 链路。
    return (
      "连不上房主的信令。同一 Wi-Fi 请重试（会自动重新发现），跨网络请让房主发邀请给你；" +
      "若地址本来就能通，可能是房主那台 App 关了或防火墙拦了。"
    );
  }
  return raw;
}

export const useVoiceStore = create<VoiceState>()(
  persist(
    (set, get) => ({
  roomId: null,
  userId: null,
  busy: false,
  error: null,
  signalError: null,
  advertiseWarning: null,
  members: [],
  speakingSelf: false,
  micLevel: 0,
  micGain: 1,
  speakerGain: 1,
  nsEnabled: true,
  agcEnabled: true,
  muted: false,
  deafened: false,
  pttEnabled: false,
  pttKey: "V",
  pttHeld: false,
  inputDevice: null,
  outputDevice: null,
  showSettings: false,
  listening: false,
  manualMode: false,
  natKind: null,
  natProbing: false,

  setManualMode: (manualMode) => set({ manualMode }),

  probeNat: async () => {
    const cached = get().natKind;
    if (cached) return cached;
    if (get().natProbing) return null; // 已有一次在飞，别叠着探
    set({ natProbing: true });
    try {
      const info = await ipc.probeNat();
      set({ natKind: info, natProbing: false });
      return info;
    } catch {
      // 探不出来不是错：判不出 ≠ 打不通，静默跳过，别拿它吓用户
      set({ natProbing: false });
      return null;
    }
  },

  setRoomInput: (roomId) => set({ roomId }),
  setError: (error) => set({ error }),
  setMuted: (muted) => {
    set({ muted });
    void ipc.setMuted(muted);
  },
  setDeafened: (deafened) => {
    set({ deafened });
    void ipc.setDeafened(deafened);
  },
  setPtt: (pttEnabled, pttKey) =>
    set({ pttEnabled, pttKey, pttHeld: pttEnabled ? get().pttHeld : false }),
  setDevices: (inputDevice, outputDevice) => set({ inputDevice, outputDevice }),
  setShowSettings: (showSettings) => set({ showSettings }),
  setMicGain: (micGain) => {
    set({ micGain });
    void ipc.setMicGain(micGain);
  },
  setSpeakerGain: (speakerGain) => {
    set({ speakerGain });
    void ipc.setSpeakerGain(speakerGain);
  },
  setNsEnabled: (nsEnabled) => {
    set({ nsEnabled });
    void ipc.setNsEnabled(nsEnabled);
  },
  setAgcEnabled: (agcEnabled) => {
    set({ agcEnabled });
    void ipc.setAgcEnabled(agcEnabled);
  },

  applyStats: () => {
    void ipc.getStats().then((s) => {
      if (!s) return;
      const st = get();
      set({
        signalError: s.signal_error,
        members: s.peers,
        speakingSelf: s.speaking_self,
        micLevel: s.mic_level,
        micGain: s.mic_gain,
        speakerGain: s.speaker_gain,
        nsEnabled: s.ns_enabled,
        agcEnabled: s.agc_enabled,
        muted: st.pttEnabled ? !st.pttHeld : s.muted,
      });
    });
  },

  startMicTest: async () => {
    const { inputDevice, outputDevice } = get();
    await ipc.micLoopbackStart({ input: inputDevice, output: outputDevice });
    set({ listening: true });
  },
  stopMicTest: async () => {
    await ipc.micLoopbackStop().catch(() => undefined);
    set({ listening: false });
  },

  joinCurrentRoom: () => doJoin(get, set),
  leaveRoom: async () => {
    await ipc.micLoopbackStop().catch(() => undefined);
    // 主动离房就该撤广播：人都不在了，还让局域网 resolve 到这台，对端只会白探一轮。
    // （服务端那边房间还会留一段宽限期，那是为了扛住"重进"的空窗，不代表这台还在。）
    await ipc.stopAdvertising().catch(() => undefined);
    await ipc.leaveRoom().catch(() => undefined);
    set({
      roomId: null,
      userId: null,
      signalError: null,
      advertiseWarning: null,
      manualMode: false,
      members: [],
      speakingSelf: false,
      listening: false,
      muted: false,
      pttHeld: false,
    });
  },
  rejoin: async () => {
    await ipc.leaveRoom().catch(() => undefined);
    await doJoin(get, set);
  },
    }),
    {
      name: "vivi",
      // 只记偏好，不记会话（房间/成员每次重进）
      partialize: (s) => ({
        inputDevice: s.inputDevice,
        outputDevice: s.outputDevice,
        pttEnabled: s.pttEnabled,
        pttKey: s.pttKey,
        micGain: s.micGain,
        speakerGain: s.speakerGain,
        nsEnabled: s.nsEnabled,
        agcEnabled: s.agcEnabled,
      }),
    },
  ),
);
