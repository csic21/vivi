# P2P 游戏语音软件开发计划

> 原始计划存档（单一事实来源）。后续补充文档见 `docs/README.md`。

## 1. 项目目标

开发一款面向游戏开黑场景的轻量级实时语音软件。

核心目标：

- 超低延迟语音
- P2P 优先通信
- NAT 穿透
- TURN 自动兜底
- Windows / macOS / Linux 跨平台
- 后续支持 Android / iOS
- 2～6 人小队语音
- 低 CPU / 内存占用
- 弱网环境下保持较好的语音质量
- UI 卡顿不能影响语音链路

第一阶段不追求 Discord 那种大型社区功能。

核心卖点只有三个：

**快、轻、稳定。**

---

## 2. 技术栈

### Client

UI：

- Tauri 2
- React 19
- TypeScript
- Vite
- Zustand

核心：

- Rust 2024
- Tokio
- CPAL

音频：

- CPAL
- Opus
- WebRTC APM
- RNNoise

实时网络：

- WebRTC
- ICE
- STUN
- TURN
- SRTP
- UDP

服务端：

- Rust
- Axum
- Tokio
- WebSocket

基础设施：

- coturn
- Docker
- GitHub Actions

---

## 3. 项目整体架构

```text
┌─────────────────────────────┐
│       Tauri Desktop         │
│                             │
│ React UI                    │
│                             │
│ 房间                        │
│ 用户                        │
│ 麦克风                      │
│ 网络状态                    │
└──────────────┬──────────────┘
               │
          Tauri IPC
               │
               ↓
┌─────────────────────────────┐
│       Rust Voice Core       │
│                             │
│ Audio Capture               │
│     ↓                       │
│ Noise Suppression           │
│     ↓                       │
│ AEC / AGC / VAD             │
│     ↓                       │
│ Opus Encode                 │
│     ↓                       │
│ WebRTC                      │
│     ↓                       │
│ ICE / UDP                   │
└──────────────┬──────────────┘
               │
       ┌───────┴────────┐
       │                │
       ↓                ↓
     P2P              TURN
       │                │
       ↓                ↓
   Player B         coturn

       ↑
       │
  Signaling Server
       │
 Rust + Axum
 WebSocket
```

核心原则：

**React 不处理实时音频。**

实时音频链路全部放到 Rust。

---

## 4. Monorepo 结构

```text
game-voice/

apps/

├── desktop/
│   ├── src/
│   │   ├── pages/
│   │   ├── components/
│   │   ├── hooks/
│   │   └── stores/
│   │
│   └── src-tauri/
│
├── signaling/
│
│   └── src/
│
crates/

├── voice-core/
│   ├── audio/
│   ├── codec/
│   ├── dsp/
│   ├── jitter/
│   ├── mixer/
│   └── stats/
│
├── voice-webrtc/
│
├── voice-protocol/
│
└── voice-common/

infra/

├── coturn/
├── docker/
└── deploy/

docs/
```

最重要的是：

```text
voice-core
```

它不能依赖：

```text
React
Tauri
UI
```

这样以后 Android、iOS、CLI 甚至服务端都可以复用。

---

## 5. 第一阶段：音频核心

目标：

**先让自己听见自己。**

不要急着做 P2P。

### Task 1

完成 CPAL 音频设备枚举。

支持：

- 获取麦克风列表
- 获取扬声器列表
- 获取默认设备
- 切换输入设备
- 切换输出设备

### Task 2

麦克风采集。

默认：

```text
Sample Rate
48000 Hz

Channel
Mono

Frame
10 ms

Samples
480
```

建立：

```text
Mic
 ↓
CPAL
 ↓
PCM
```

### Task 3

音频播放。

实现：

```text
Mic
 ↓
Capture
 ↓
Playback
 ↓
Speaker
```

形成最简单的 loopback。

### Task 4

测量音频基础延迟。

输出：

```text
Input latency
Output latency
Buffer size
Sample rate
```

第一阶段完成标准：

**Windows / macOS 能稳定采集 + 播放语音。**

---

## 6. 第二阶段：Opus

目标：

把 PCM 音频转换成适合网络传输的数据。

链路：

```text
PCM
 ↓
Opus Encode
 ↓
Opus Packet
 ↓
Opus Decode
 ↓
PCM
```

默认配置：

```text
48KHz
Mono
10ms

24~48Kbps
```

推荐默认：

```text
32Kbps
```

需要支持：

- bitrate 调整
- DTX
- FEC
- packet loss percentage
- complexity

建立 benchmark：

```text
encode latency

decode latency

CPU usage
```

目标：

单次 Encode / Decode 尽量控制在：

```text
< 2ms
```

---

## 7. 第三阶段：网络 MVP

这一阶段暂时不做 WebRTC。

先自己实现最简单 UDP：

```text
Client A

Mic
 ↓
Opus
 ↓
UDP
 ↓

Client B

UDP
 ↓
Opus
 ↓
Speaker
```

Packet：

```text
VoicePacket

version

user_id

sequence

timestamp

payload
```

例如：

```rust
struct VoicePacket {
    version: u8,
    user_id: u64,
    sequence: u32,
    timestamp: u64,
    payload: Vec<u8>,
}
```

实现：

- Sequence Number
- Timestamp
- Packet Loss 统计
- RTT 统计
- Jitter 统计

这一阶段非常重要。

因为你可以真正理解：

**实时语音到底在传什么。**

---

## 8. 第四阶段：Jitter Buffer

网络一定会发生：

```text
Packet 1

Packet 3

Packet 2

Packet 5

Packet 4
```

所以需要：

```text
UDP
 ↓
Jitter Buffer
 ↓
Opus Decoder
```

实现：

- packet reorder
- late packet discard
- packet loss detection
- adaptive jitter buffer

初始：

```text
20ms
```

可调整：

```text
10ms
20ms
30ms
40ms
```

目标：

网络良好：

```text
10~20ms
```

网络较差：

```text
30~60ms
```

---

## 9. 第五阶段：WebRTC / P2P

UDP MVP 稳定以后，再接 WebRTC。

实现：

```text
Peer A
 ↓
ICE
 ↓
STUN
 ↓
NAT Traversal
 ↓
Peer B
```

需要：

- ICE Candidate
- SDP
- DTLS
- SRTP
- STUN

目标：

两个不同网络的客户端可以直接建立：

```text
P2P UDP
```

---

## 10. 第六阶段：信令服务器

实现：

```text
Rust
+
Axum
+
WebSocket
```

服务器只负责：

```text
登录

创建房间

加入房间

离开房间

交换 SDP

交换 ICE Candidate

维护在线状态
```

不负责传输语音。

API：

```text
POST /rooms

GET /rooms/:id

WS /signal
```

WebSocket Message：

```text
JoinRoom

LeaveRoom

Offer

Answer

IceCandidate

PeerJoined

PeerLeft
```

---

## 11. 第七阶段：TURN

部署：

```text
coturn
```

连接策略：

```text
P2P
 │
 │ failed
 ↓
TURN
```

UI 显示：

```text
Network

DIRECT
```

或者：

```text
RELAY
```

推荐状态颜色：

```text
🟢 DIRECT

🟡 TURN

🔴 BAD NETWORK
```

---

## 12. 第八阶段：多人语音

第一版采用：

```text
Mesh P2P
```

最多：

```text
6 人
```

例如 4 人：

```text
A ─ B
|\ /|
| X |
|/ \|
C ─ D
```

每个客户端负责：

```text
PeerConnection A

PeerConnection B

PeerConnection C
```

接收到多个音频流后：

```text
User B
 ↓

User C
 ↓
Mixer
 ↓

User D
 ↓

Speaker
```

支持单独调节：

```text
B 100%

C 40%

D 80%
```

---

## 13. 第九阶段：语音处理

增加：

### VAD

检测：

```text
正在说话
```

UI：

```text
Player A 🟢
```

### Noise Suppression

过滤：

- 风扇
- 空调
- 机械键盘
- 环境噪音

### AEC

解决：

```text
扬声器声音
 ↓
麦克风
 ↓
再次传给队友
```

也就是回声。

### AGC

自动调整麦克风音量。

最终：

```text
Mic

 ↓

AEC

 ↓

Noise Suppression

 ↓

AGC

 ↓

VAD

 ↓

Opus
```

---

## 14. 第十阶段：Push To Talk

游戏语音非常重要。

支持：

```text
Voice Activity

Push To Talk

Always On
```

PTT 必须使用：

**系统级 Global Hotkey。**

例如：

```text
Mouse 4

Mouse 5

CapsLock

Alt

V
```

要求：

即使游戏全屏：

```text
PTT
```

仍然可以工作。

---

## 15. 第一版 UI

主页：

```text
GameVoice

Create Room

Join Room
```

房间：

```text
Room

K
████████

Player 2
████

Player 3
██████████
```

底部：

```text
🎤 Mute

🎧 Deafen

⚙ Settings
```

右上角：

```text
PING     32ms

JITTER    6ms

LOSS      0%

ROUTE     P2P
```

---

## 16. 性能指标

整个项目需要一直监控：

### CPU

Idle：

```text
< 1%
```

语音：

```text
< 5%
```

目标是现代 CPU 上保持非常轻量。

### Memory

目标：

```text
< 100MB
```

理想：

```text
30~70MB
```

### 延迟

目标：

局域网：

```text
< 40ms
```

同城：

```text
< 60ms
```

正常互联网：

```text
< 100ms
```

体验上限：

```text
< 120ms
```

---

## 17. MVP 功能

第一版只做：

- 创建房间
- 房间 ID
- 加入房间
- 2～6 人
- 麦克风选择
- 扬声器选择
- Mute
- Deafen
- Push To Talk
- Voice Activity
- 用户音量
- Opus
- P2P
- STUN
- TURN
- Ping
- Jitter
- Packet Loss

暂时不要：

- 好友
- 私聊
- 群组
- 社区
- 游戏 Overlay
- 视频
- 屏幕共享
- 云录音
- 20+ 人频道

避免第一版项目失控。

---

## 18. 开发时间规划

### Week 1

音频基础。

完成：

```text
CPAL

Device List

Mic Capture

Audio Playback
```

里程碑：

**本机 Loopback 成功。**

---

### Week 2

Opus。

完成：

```text
PCM

↓

Opus Encode

↓

Opus Decode

↓

PCM
```

增加 benchmark。

里程碑：

**实时 Opus 编解码稳定。**

---

### Week 3

UDP 网络。

完成：

```text
Client A
      ↓
     UDP
      ↓
Client B
```

增加：

```text
Sequence

Timestamp

RTT

Packet Loss
```

里程碑：

**两个电脑可以语音。**

---

### Week 4

Jitter Buffer。

完成：

```text
Packet reorder

Packet loss

Adaptive buffer

FEC
```

里程碑：

**普通互联网环境语音稳定。**

---

### Week 5

WebRTC。

完成：

```text
ICE

STUN

P2P

SRTP
```

里程碑：

**不同 NAT 网络之间成功通话。**

---

### Week 6

信令 + TURN。

完成：

```text
Room

WebSocket

Offer

Answer

ICE Candidate

coturn
```

里程碑：

**用户输入 Room ID 即可开黑。**

---

### Week 7

多人语音。

完成：

```text
2~6 Player

Audio Mixer

Per-user volume

Mute
```

里程碑：

**完整 5 人游戏语音。**

---

### Week 8

产品体验。

完成：

```text
AEC

NS

AGC

VAD

PTT

Network Stats
```

里程碑：

**MVP 可实际开黑使用。**

---

## 19. 第二阶段规划

MVP 稳定以后：

### V0.2

增加：

```text
账号

好友

固定房间

最近房间

自动更新
```

### V0.3

增加：

```text
Game Overlay

游戏内玩家列表

游戏内 Mute

游戏内音量调整
```

### V0.4

增加：

```text
Linux

Android

iOS
```

### V0.5

研究：

```text
SFU
```

支持：

```text
10+

20+

50+
```

人数房间。

---

## 20. 项目最重要的开发原则

第一：

**Audio Thread 永远不能阻塞。**

Audio Callback 中禁止：

```text
网络请求

磁盘 IO

Mutex 长时间等待

日志刷盘

复杂 allocation
```

使用：

```text
RingBuffer

Channel

Lock-free Queue
```

传递音频数据。

第二：

**UI 和 Voice Engine 完全隔离。**

即使 React：

```text
卡死 2 秒
```

语音也必须：

```text
完全正常
```

第三：

**优先降低 jitter，而不是只看平均延迟。**

游戏语音：

```text
40ms
45ms
42ms
47ms
```

通常比：

```text
20ms
80ms
25ms
100ms
```

体验好得多。

第四：

**每一步都做 Benchmark。**

持续记录：

```text
Capture latency

Encode latency

Network latency

Jitter Buffer

Decode latency

Playback latency
```

最终做成：

```text
Mic
10ms

Encode
1ms

Network
28ms

Jitter
20ms

Decode
1ms

Playback
10ms

Total
70ms
```

这会成为整个项目最重要的技术指标。

---

## 21. 第一个正式版本定义

版本：

```text
GameVoice v0.1
```

完成标准：

**两个玩家：**

打开软件 →

创建房间 →

复制 Room ID →

朋友加入 →

自动 NAT 穿透 →

优先建立 P2P →

失败自动 TURN →

开始语音。

支持：

```text
2~6 人

<100ms 典型延迟

Push To Talk

Noise Suppression

Mic Select

Speaker Select

Individual Volume

Packet Loss

Ping

Jitter

P2P / TURN Status
```

做到这个程度，就已经不是技术 Demo，而是一款真正可以每天拿来开黑的软件。
