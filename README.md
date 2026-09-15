# Vivi v0.3.1

<img src="brand/icon-source.png" alt="Vivi" width="96" height="96" />

[![CI](https://github.com/csic21/vivi/actions/workflows/ci.yml/badge.svg)](https://github.com/csic21/vivi/actions/workflows/ci.yml)
[![Release Desktop](https://github.com/csic21/vivi/actions/workflows/release-desktop.yml/badge.svg)](https://github.com/csic21/vivi/actions/workflows/release-desktop.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

面向游戏开黑的轻量级实时语音：快、轻、稳定。

## 原则

- `voice-core` 不依赖 React / Tauri / UI，可被桌面 / CLI / 移动端复用。
- 实时音频链路全在 Rust，React 只做房间 / 用户 / 设置 / 状态展示。
- Audio callback 中禁止网络 / 磁盘 IO / 长时间 Mutex / 日志刷盘 / 复杂 allocation，用 RingBuffer + Channel 传递数据。

## 仓库结构

```text
apps/desktop        Tauri 2 + React 19 + TS + Zustand（UI，不碰实时音频）
apps/desktop/src-tauri  独立 Cargo crate（excluded from root workspace），通过 IPC 调用 voice-core
apps/signaling      Axum + WebSocket 信令（只做房间/SDP/ICE，不传语音）
crates/voice-common 共享类型（音频配置/设备/网络状态）
crates/voice-protocol 信令消息 + VoicePacket（UDP MVP 包格式）
crates/voice-core   音频采集/编解码/DSP/jitter/mixer/stats（核心）
crates/voice-webrtc P2P 封装（Phase 5，当前 stub）
infra/coturn        coturn 配置
infra/docker        本地联调 compose
docs/               补充文档
```

## 端到端延迟预算（Section 20）

```text
Mic 10ms + Encode 1ms + Network 28ms + Jitter 20ms + Decode 1ms + Playback 10ms = Total 70ms
```

## 快速开始

```bash
# 桌面端（自带内嵌信令，开箱即用；如需外置信令见下）
pnpm --dir apps/desktop tauri dev
```

桌面 App 启动时会在本机 127.0.0.1:8080 起一个内嵌信令（建房/入会/TURN 凭证全套），
8080 被占用则自动让路给已有的。跨机器联调时，被加入方把
`VITE_SIGNALING_URL` 指向房主 IP 即可（如 `http://192.168.1.10:8080`）。

```bash
# Rust workspace 检查（含 signaling + voice-*）
cargo check --workspace

# signaling 单独跑（桌面内嵌已够用，一般不需要）
cargo run -p signaling
```

## 发版

推送 tag 自动构建三平台安装包并发布，客户端启动时自动提示更新。
完整流程见 [docs/release.md](./docs/release.md)：

```bash
node apps/desktop/scripts/check-versions.mjs  # 发版前自查版本号
git tag v0.2.0 && git push origin main v0.2.0          # 桌面端
git tag signaling-v0.2.0 && git push origin signaling-v0.2.0  # 信令（二进制）
```

## License

MIT © 2026 csic21，见 [LICENSE](./LICENSE)。
