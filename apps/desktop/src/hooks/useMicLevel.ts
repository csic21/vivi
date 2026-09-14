import { useEffect, useRef, useState } from "react";
import { ipc } from "../ipc";
import { useVoiceStore } from "../stores/useVoiceStore";

/** RMS 0..1 → 电平表百分比（对数压缩）。 */
export function levelToPct(level: number): number {
  const db = 20 * Math.log10(Math.max(1e-4, level));
  return Math.round(Math.min(1, Math.max(0, (db + 60) / 60)) * 100);
}

/**
 * 本端 mic 实时电平（50ms 轮询后端原子值），带真表头弹道：
 * 起得快（0.6）、落得慢（0.15），说话断句时表针自然回落不乱跳。
 * 只在房间内跑；离开房间自动停并归零。
 * `stale` 为 true 表示连调 5 次都失败（后端太旧/进程不对），UI 应提示重启。
 */
export function useMicLevel(): { level: number; stale: boolean } {
  const roomId = useVoiceStore((s) => s.roomId);
  const [level, setLevel] = useState(0);
  const [fails, setFails] = useState(0);
  const shown = useRef(0);

  useEffect(() => {
    if (!roomId) {
      shown.current = 0;
      setLevel(0);
      setFails(0);
      return;
    }
    let alive = true;
    let f = 0;
    const timer = setInterval(() => {
      ipc
        .micLevel()
        .then((v) => {
          if (!alive) return;
          f = 0;
          setFails(0);
          const d = shown.current;
          shown.current = v > d ? d + (v - d) * 0.6 : d + (v - d) * 0.15;
          setLevel(shown.current);
        })
        .catch(() => {
          if (!alive) return;
          f += 1;
          setFails(f);
        });
    }, 50);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [roomId]);

  return { level, stale: fails >= 5 };
}
