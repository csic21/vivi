//! 信令消息 + UDP MVP 语音包（Section 7 / Section 10）。
//! signaling 只透传这些消息，不解析语音 payload。
//!
//! 线格式（首字节分发）：`1` = 语音包，`2` = 控制包（Ping/Pong 测 RTT）。

use serde::{Deserialize, Serialize};
use voice_common::UserId;

pub const VOICE_PACKET_VERSION: u8 = 1;
pub const CONTROL_PACKET_VERSION: u8 = 2;
/// 语音头长度：[version:1][user_id:8 LE][seq:4 LE][ts:8 LE]
pub const VOICE_HEADER_LEN: usize = 21;
/// 收包缓冲建议大小：语音头 21 + Opus 单包（远小于 4000）。
pub const MAX_UDP_DATAGRAM: usize = 4096;

/// Section 7 最简 UDP 语音包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoicePacket {
    pub version: u8,
    pub user_id: u64,
    pub sequence: u32,
    pub timestamp_ms: u64,
    pub payload: Vec<u8>,
}

impl VoicePacket {
    pub fn new(user_id: u64, sequence: u32, timestamp_ms: u64, payload: Vec<u8>) -> Self {
        Self {
            version: VOICE_PACKET_VERSION,
            user_id,
            sequence,
            timestamp_ms,
            payload,
        }
    }

    /// wire: [version:1][user_id:8 LE][seq:4 LE][ts:8 LE][payload..]
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(VOICE_HEADER_LEN + self.payload.len());
        out.push(self.version);
        out.extend_from_slice(&self.user_id.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_le_bytes());
        out.extend_from_slice(&self.payload);
    }

    /// 零分配编码（音频热路径用）：返回写入字节数，缓冲不足返回 `None`。
    pub fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        Self::encode_parts(
            self.user_id,
            self.sequence,
            self.timestamp_ms,
            &self.payload,
            out,
        )
    }

    /// 零分配组包：直接拼头 + payload 切片（发包侧连 `VoicePacket` 都不用构造）。
    pub fn encode_parts(
        user_id: u64,
        sequence: u32,
        timestamp_ms: u64,
        payload: &[u8],
        out: &mut [u8],
    ) -> Option<usize> {
        let n = VOICE_HEADER_LEN + payload.len();
        if out.len() < n {
            return None;
        }
        out[0] = VOICE_PACKET_VERSION;
        out[1..9].copy_from_slice(&user_id.to_le_bytes());
        out[9..13].copy_from_slice(&sequence.to_le_bytes());
        out[13..21].copy_from_slice(&timestamp_ms.to_le_bytes());
        out[21..n].copy_from_slice(payload);
        Some(n)
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        decode_view(buf).map(|v| Self {
            version: VOICE_PACKET_VERSION,
            user_id: v.user_id,
            sequence: v.sequence,
            timestamp_ms: v.timestamp_ms,
            payload: v.payload.to_vec(),
        })
    }
}

/// 零拷贝语音包视图（收包热路径用，不分配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceView<'a> {
    pub user_id: u64,
    pub sequence: u32,
    pub timestamp_ms: u64,
    pub payload: &'a [u8],
}

pub fn decode_view(buf: &[u8]) -> Option<VoiceView<'_>> {
    if buf.len() < VOICE_HEADER_LEN || buf[0] != VOICE_PACKET_VERSION {
        return None;
    }
    Some(VoiceView {
        user_id: u64::from_le_bytes(buf[1..9].try_into().ok()?),
        sequence: u32::from_le_bytes(buf[9..13].try_into().ok()?),
        timestamp_ms: u64::from_le_bytes(buf[13..21].try_into().ok()?),
        payload: &buf[VOICE_HEADER_LEN..],
    })
}

/// RTT 测量控制包，与语音包同 socket 传输。
/// wire: [version:1 = 2][kind:1 0=Ping 1=Pong][seq:4 LE][send_ms:8 LE]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPacket {
    Ping { seq: u32, send_ms: u64 },
    Pong { seq: u32, send_ms: u64 },
}

impl ControlPacket {
    pub const WIRE_LEN: usize = 14;

    /// 零分配编码：返回写入字节数，缓冲不足返回 `None`。
    pub fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        if out.len() < Self::WIRE_LEN {
            return None;
        }
        let (kind, seq, send_ms) = match *self {
            Self::Ping { seq, send_ms } => (0u8, seq, send_ms),
            Self::Pong { seq, send_ms } => (1u8, seq, send_ms),
        };
        out[0] = CONTROL_PACKET_VERSION;
        out[1] = kind;
        out[2..6].copy_from_slice(&seq.to_le_bytes());
        out[6..14].copy_from_slice(&send_ms.to_le_bytes());
        Some(Self::WIRE_LEN)
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::WIRE_LEN || buf[0] != CONTROL_PACKET_VERSION {
            return None;
        }
        let seq = u32::from_le_bytes(buf[2..6].try_into().ok()?);
        let send_ms = u64::from_le_bytes(buf[6..14].try_into().ok()?);
        match buf[1] {
            0 => Some(Self::Ping { seq, send_ms }),
            1 => Some(Self::Pong { seq, send_ms }),
            _ => None,
        }
    }
}

/// Section 10 WebSocket 信令消息。
///
/// Mesh 入会协议（防 offer 冲突 / glare）：
/// 新人收到 `PeerJoined(老人)` 后由**新人**向老人发起 offer；老人只等 offer 不主动发。
/// `from` 由服务端按连接身份覆盖，客户端填什么都会被纠正（防冒充）。
///
/// 队友中继协议（A—B 直连失败，B 经 C 中转 A 的语音）：
/// B → C `RelayRequest{target: A}`；C 有 A 的活路则回 `RelayAccepted` 并开始转发，
/// 否则 `RelayRejected`；直连恢复或离会时 B 发 `RelayStop`。中继包走 C—B 的
/// DataChannel，包络复用 VoicePacket 线格式（user_id = 源 A）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalMessage {
    JoinRoom {
        room_id: String,
        user_id: UserId,
    },
    LeaveRoom {
        room_id: String,
        user_id: UserId,
    },
    Offer {
        from: UserId,
        to: UserId,
        sdp: String,
    },
    Answer {
        from: UserId,
        to: UserId,
        sdp: String,
    },
    IceCandidate {
        from: UserId,
        to: UserId,
        candidate: String,
    },
    RelayRequest {
        from: UserId,
        to: UserId,
        target: UserId,
    },
    RelayAccepted {
        from: UserId,
        to: UserId,
        target: UserId,
    },
    RelayRejected {
        from: UserId,
        to: UserId,
        target: UserId,
    },
    RelayStop {
        from: UserId,
        to: UserId,
        target: UserId,
    },
    PeerJoined {
        user_id: UserId,
    },
    PeerLeft {
        user_id: UserId,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_packet_roundtrip() {
        let p = VoicePacket::new(7, 42, 1_000, vec![1, 2, 3]);
        let mut buf = Vec::new();
        p.encode(&mut buf);
        assert_eq!(VoicePacket::decode(&buf), Some(p));
    }

    #[test]
    fn voice_packet_encode_into_should_match_vec_encode() {
        let p = VoicePacket::new(9, 77, 5_000, vec![4, 5, 6, 7]);
        let mut flat = [0u8; MAX_UDP_DATAGRAM];
        let n = p.encode_into(&mut flat).unwrap();
        let mut vec = Vec::new();
        p.encode(&mut vec);
        assert_eq!(&flat[..n], &vec[..]);
    }

    #[test]
    fn voice_packet_encode_into_should_reject_small_buffer() {
        let p = VoicePacket::new(1, 1, 1, vec![0u8; 100]);
        let mut tiny = [0u8; 10];
        assert_eq!(p.encode_into(&mut tiny), None);
    }

    #[test]
    fn control_packet_roundtrip() {
        let ping = ControlPacket::Ping {
            seq: 3,
            send_ms: 999,
        };
        let mut buf = [0u8; ControlPacket::WIRE_LEN];
        assert_eq!(ping.encode_into(&mut buf), Some(ControlPacket::WIRE_LEN));
        assert_eq!(ControlPacket::decode(&buf), Some(ping));

        let pong = ControlPacket::Pong {
            seq: 3,
            send_ms: 999,
        };
        pong.encode_into(&mut buf).unwrap();
        assert_eq!(ControlPacket::decode(&buf), Some(pong));
    }

    #[test]
    fn decode_view_should_borrow_without_alloc() {
        let p = VoicePacket::new(2, 8, 300, vec![9, 9]);
        let mut buf = Vec::new();
        p.encode(&mut buf);
        let v = decode_view(&buf).unwrap();
        assert_eq!((v.user_id, v.sequence, v.timestamp_ms), (2, 8, 300));
        assert_eq!(v.payload, &[9, 9]);
    }

    #[test]
    fn signal_message_json_shape() {
        let m = SignalMessage::JoinRoom {
            room_id: "abc".into(),
            user_id: UserId(1),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["type"], "join_room");
    }
}
