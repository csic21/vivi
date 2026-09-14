//! Week 7 Mesh 通话编排：信令入会 + 每对端 VoicePeer + 单混音回路。
//!
//! 音频流（全双工）：
//! ```text
//! mic → pump 线程 → encode 一次 → 分发给 N 个 VoicePeer::send_opus
//! 每对端 remote_frames → 该对端 JB → 10ms 总 tick → 各自解码 → Mixer(每用户增益) → 扬声器
//! ```
//! 单 tick 任务串行处理全部对端；JB 用短临界区 Mutex 在收包/pump 间共享。

pub mod session;
pub mod signaling_client;
pub mod stats;

pub use session::{AudioMode, AudioSettings, Session, SessionConfig, SessionHandle};
pub use stats::{PeerStats, SessionStats};
