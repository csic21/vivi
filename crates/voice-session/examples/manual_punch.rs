//! 手动（剪贴板）打洞验证器：不连信令服务器，两人靠粘贴连接码连上。
//!
//! 用来把「牵线」和「打洞」两个变量彻底分开：房间模式异地连不上时，你没法判断
//! 是信令没通、还是 ICE 打不穿。这个工具里信令压根不存在（码是手搬的），
//! 所以连不上就只剩打洞这一个原因。
//!
//! ```sh
//! # 房主（id 1，发 offer）—— 先跑
//! cargo run --release -p voice-session --example manual_punch -- offer
//!
//! # 队友（id 2，回 answer）
//! cargo run --release -p voice-session --example manual_punch -- answer
//! ```
//!
//! 流程：`offer` 侧先出一段码 → 发给对方 → 对方粘进来，出一段码发回来 →
//! 粘回来。两边都看到「已连通」就能说话了。
//!
//! STUN 走 `VIVI_STUN_URLS`（逗号分隔），与桌面端同一套默认值。
//! `--synthetic` 用合成音频（无麦无喇叭也能验连通性，CI/无头环境用）。

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use voice_session::manual::{self, DEFAULT_GATHER_TIMEOUT};
use voice_session::nat;
use voice_session::{AudioMode, GatherOutcome, Session, SessionConfig, SessionHandle, Signaling};
use voice_webrtc::PeerConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let synthetic = args.iter().any(|a| a == "--synthetic");
    let role = match args.iter().find(|a| *a == "offer" || *a == "answer") {
        Some(r) => r.clone(),
        None => {
            eprintln!("用法：manual_punch <offer|answer> [--synthetic]");
            eprintln!("  offer  = 房主，先跑，出第一段连接码");
            eprintln!("  answer = 队友，等房主的码，粘进来后出第二段");
            std::process::exit(2);
        }
    };

    // id 小的发 offer —— 沿用房间模式的确定性规则，两边算出同一结论
    let (my_id, peer_id) = if role == "offer" {
        (1u64, 2u64)
    } else {
        (2u64, 1u64)
    };

    let peer_config = PeerConfig {
        stun_urls: stun_urls(),
        ..PeerConfig::default()
    };
    let nat_urls = peer_config.stun_urls.clone();
    eprintln!(
        "角色={role} 我方 id={my_id} 对端 id={peer_id} STUN={:?}",
        peer_config.stun_urls
    );

    let (handle, mut link) = Session::join_manual(SessionConfig {
        user_id: my_id,
        room_id: String::new(),
        signaling: Signaling::Manual { peer_id },
        peer_config,
        audio: if synthetic {
            AudioMode::Synthetic
        } else {
            AudioMode::Live {
                input: None,
                output: None,
            }
        },
        relay_timeout_secs: 15,
        no_direct: vec![],
    })
    .await?;

    // 判一下本机 NAT。**故意放在 join 之后**：offer 侧那会儿 ICE gathering 正在跑，
    // 探测跟它并行，等于不额外花时间。
    report_nat(&nat_urls).await;

    if role == "offer" {
        // 房主先出码（offer 在入会时就建好了），再等对方的回答
        our_code(&mut link).await?;
        let theirs = read_code("把队友的连接码粘进来，然后回车：").await?;
        eprintln!("对端连接码：{}", summarize(&theirs));
        link.feed(&theirs).await?;
    } else {
        // 队友反过来：先收房主的 offer，才能出自己的 answer
        let theirs = read_code("把房主的连接码粘进来，然后回车：").await?;
        eprintln!("对端连接码：{}", summarize(&theirs));
        link.feed(&theirs).await?;
        our_code(&mut link).await?;
    }

    wait_connected(&handle).await?;
    run_until_quit(&handle).await;
    handle.leave().await;
    eprintln!("已离会。");
    Ok(())
}

/// 判本机 NAT 并打出来。
///
/// 对称型是决定性的坏消息：出口端口随目标变，对方无从预测该往哪儿打，
/// **打洞必失败**。早点说出来，比让人对着一个永远不会通的界面猜要好。
async fn report_nat(urls: &[String]) {
    let kind = nat::classify(urls, nat::PER_SERVER).await;
    eprintln!("本机 NAT：{kind}");
    if !kind.punching_may_work() {
        eprintln!(
            "⚠️  对称 NAT 下打洞必然失败。下面还能继续跑，但那是为了确认码的交换\n\
             \x20   流程本身通不通 —— 真要连上得靠 TURN，见 docs/NETWORK.md 第 4 节。"
        );
    }
}

/// 收齐本端候选 → 打印连接码。
async fn our_code(link: &mut voice_session::ManualLink) -> anyhow::Result<String> {
    eprintln!(
        "正在收集 ICE 候选（最多 {}s）…",
        DEFAULT_GATHER_TIMEOUT.as_secs()
    );
    let started = std::time::Instant::now();
    let (code, outcome) = link.next_code(DEFAULT_GATHER_TIMEOUT).await?;
    // 把收工方式打出来：配了 STUN 时几乎总是 Settled，看不到这个会以为是 bug
    let how = match outcome {
        GatherOutcome::Complete => "webrtc 报了 gathering 完成",
        GatherOutcome::Settled => "候选静默收敛（配了 STUN 时的常态）",
        GatherOutcome::TimedOut => "硬超时截断",
    };
    eprintln!(
        "本端连接码：{}（耗时 {:.1}s，{how}）",
        summarize(&code),
        started.elapsed().as_secs_f32()
    );
    if !outcome.is_usable() {
        eprintln!(
            "⚠️  候选没在 {}s 内收齐，这份码不一定打得通。可以先试，不行就重跑一次。",
            DEFAULT_GATHER_TIMEOUT.as_secs()
        );
    }
    print_code(&code);
    Ok(code)
}

/// 连接码打成一眼能粘走的形状，同时落一份文件（长码粘终端容易出事）。
fn print_code(code: &str) {
    let path = std::env::temp_dir().join("vivi-manual-code.txt");
    let saved = std::fs::write(&path, code).is_ok();
    println!("\n===== 连接码：发给对方 =====\n{code}\n===== 结束 =====");
    if saved {
        println!("（同样的内容也写在 {}，可以直接发文件）\n", path.display());
    }
}

/// 连接码里有哪些候选 —— 打洞能不能成，看的就是这里有没有 srflx。
fn summarize(code: &str) -> String {
    let Ok(s) = manual::summarize_code(code) else {
        return "解不开（码可能被截断了）".into();
    };
    let mut out = format!("SDP {}B；{}", s.sdp_bytes, s.describe());
    if !s.can_cross_nat() {
        out.push_str("  ← 没有 srflx/relay：STUN 没出结果，跨 NAT 大概率打不通");
    }
    out
}

/// 读一段连接码。
///
/// 逐行累积到能解出来为止，而不是只读一行：终端对长粘贴的折行行为没法指望，
/// 而 `manual::decode` 会先把空白剥干净，所以「折了行的码」照样能认。
async fn read_code(prompt: &str) -> anyhow::Result<String> {
    eprintln!("{prompt}");
    let mut acc = String::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        acc.push_str(&line);
        if manual::decode(&acc).is_ok() {
            return Ok(acc);
        }
        if !acc.starts_with(manual::CODE_PREFIX) {
            // 一眼就不是码（比如敲了别的命令），别让用户对着死掉的输入干等
            anyhow::bail!("输入不是 Vivi 连接码（应以 {} 开头）", manual::CODE_PREFIX);
        }
    }
    anyhow::bail!("还没读到完整的连接码，输入就结束了（Ctrl-D）")
}

async fn wait_connected(handle: &SessionHandle) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(p) = handle.stats().peers.first() {
            if p.connected {
                eprintln!("✅ 已连通（route={:?}），开始说话，Ctrl-C 退出", p.route);
                return Ok(());
            }
        }
        if tokio::time::Instant::now() > deadline {
            eprintln!("最后快照：{:?}", handle.stats());
            anyhow::bail!(
                "45s 内没连上。信令在这条路上是不存在的（码是手搬的），\
                 所以问题在打洞本身：多半是对称 NAT —— 那就得靠 TURN 了，见 docs/NETWORK.md 第 4 节"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn run_until_quit(handle: &SessionHandle) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tick.tick() => {
                let s = handle.stats();
                if s.peers.is_empty() {
                    eprintln!("(还没有对端槽位)");
                    continue;
                }
                for p in &s.peers {
                    eprintln!(
                        "peer{} connected={} route={:?} rtt={:?}ms jitter={:.1}ms loss={:.1}% dec={} plc={}",
                        p.user_id, p.connected, p.route, p.rtt_ms,
                        p.jitter_ms, p.loss_percent, p.decoded_frames, p.plc_frames
                    );
                }
                eprintln!(
                    "本端 mic_level={:.3} speaking={} mixed={}",
                    s.mic_level, s.speaking_self, s.mixed_frames
                );
            }
        }
    }
}

/// 与桌面端同源：`VIVI_STUN_URLS` 覆盖，否则用 `PeerConfig::default()` 那一组。
fn stun_urls() -> Vec<String> {
    std::env::var("VIVI_STUN_URLS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| PeerConfig::default().stun_urls)
}
