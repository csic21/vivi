import { useEffect, useState } from "react";
import { ipc } from "../ipc";
import { useVoiceStore } from "../stores/useVoiceStore";

/** RMS 0..1 → 电平表百分比（对数压缩）。 */
export function levelToPct(level: number): number {
  const db = 20 * Math.log10(Math.max(1e-4, level));
  return Math.round(Math.min(1, Math.max(0, (db + 60) / 60)) * 100);
}

/** 话筒亮起门限：低于此视为环境底噪，避免麦标常闪。 */
export function levelIsHot(level: number): boolean {
  return levelToPct(level) >= 12;
}

type Listener = (level: number, stale: boolean) => void;

const listeners = new Set<Listener>();
let timer: ReturnType<typeof setInterval> | null = null;
let shown = 0;
let fails = 0;

function startPoll() {
  if (timer != null) return;
  fails = 0;
  shown = 0;
  timer = setInterval(() => {
    ipc
      .micLevel()
      .then((v) => {
        fails = 0;
        shown = v > shown ? shown + (v - shown) * 0.6 : shown + (v - shown) * 0.15;
        for (const fn of listeners) fn(shown, false);
      })
      .catch(() => {
        fails += 1;
        const stale = fails >= 5;
        for (const fn of listeners) fn(shown, stale);
      });
  }, 50);
}

function stopPoll() {
  if (timer == null) return;
  clearInterval(timer);
  timer = null;
  shown = 0;
  fails = 0;
}

/**
 * 本端 mic 实时电平（50ms 轮询后端原子值），带真表头弹道：
 * 起得快（0.6）、落得慢（0.15），说话断句时表针自然回落不乱跳。
 * 房间内或试麦中才跑；多处订阅共用一个定时器。
 */
export function useMicLevel(): { level: number; stale: boolean } {
  const enabled = useVoiceStore((s) => Boolean(s.roomId) || s.listening);
  const [level, setLevel] = useState(0);
  const [stale, setStale] = useState(false);

  useEffect(() => {
    if (!enabled) {
      setLevel(0);
      setStale(false);
      return;
    }
    const fn: Listener = (v, s) => {
      setLevel(v);
      setStale(s);
    };
    listeners.add(fn);
    startPoll();
    return () => {
      listeners.delete(fn);
      if (listeners.size === 0) stopPoll();
    };
  }, [enabled]);

  return { level, stale };
}
