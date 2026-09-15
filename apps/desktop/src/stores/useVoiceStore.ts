import { create } from "zustand";
import { persist } from "zustand/middleware";
import { ipc } from "../ipc";
import { checkRoomExists, currentSignalingWs, fetchTurnCreds, normalizeRoomId } from "../api/signaling";
import type { PeerStats } from "../types";

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
  set({ busy: true, error: null });
  try {
    // 预检：房号错 / 信令不是同一个，直接报错，不进房空等
    await checkRoomExists(rid);
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
    return "连不上信令：确认 App 在跑（本机 8080 被占用时会让路给已有的）；跨机器联调检查地址对不对。";
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
    await ipc.leaveRoom().catch(() => undefined);
    set({
      roomId: null,
      userId: null,
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
