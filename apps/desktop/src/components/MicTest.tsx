import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { levelIsHot, levelToPct, useMicLevel } from "../hooks/useMicLevel";
import { MicIcon } from "./icons";

/**
 * 试麦：点一下开始环回，对着话筒说话时图标跟着亮。
 * compact 用在房间设置里，默认是首页主卡片。
 */
export function MicTest({ compact = false }: { compact?: boolean }) {
  const listening = useVoiceStore((s) => s.listening);
  const startMicTest = useVoiceStore((s) => s.startMicTest);
  const stopMicTest = useVoiceStore((s) => s.stopMicTest);
  const inputDevice = useVoiceStore((s) => s.inputDevice);
  const outputDevice = useVoiceStore((s) => s.outputDevice);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { level, stale } = useMicLevel();
  const pct = levelToPct(level);
  const hot = listening && levelIsHot(level);
  const prevDev = useRef({ inputDevice, outputDevice });

  useEffect(() => {
    return () => {
      if (useVoiceStore.getState().listening) {
        void useVoiceStore.getState().stopMicTest();
      }
    };
  }, []);

  useEffect(() => {
    if (!listening) {
      prevDev.current = { inputDevice, outputDevice };
      return;
    }
    const prev = prevDev.current;
    if (prev.inputDevice === inputDevice && prev.outputDevice === outputDevice) return;
    prevDev.current = { inputDevice, outputDevice };
    void startMicTest().catch((e) => {
      setErr(`切换设备失败：${String(e)}`);
      void stopMicTest();
    });
  }, [inputDevice, outputDevice, listening, startMicTest, stopMicTest]);

  const toggle = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      if (listening) await stopMicTest();
      else await startMicTest();
    } catch (e) {
      setErr(`试麦失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const status = stale
    ? "电平无数据：重启 tauri dev"
    : !listening
      ? "对着话筒说几句，图标会跟着亮起来"
      : hot
        ? "听到了，话筒是好的"
        : "正在听你说话…";

  return (
    <section
      className={compact ? "mictest compact" : "mictest"}
      aria-label="试麦"
    >
      <div
        className={["orb", listening ? "live" : "", hot ? "hot" : ""].filter(Boolean).join(" ")}
        style={{ ["--lvl"]: String(pct / 100) } as CSSProperties}
      >
        <span className="orb-ring" />
        <span className="orb-ring late" />
        <span className="orb-core">
          <MicIcon lit={hot} level={listening ? level : 0} size={compact ? 26 : 32} />
        </span>
      </div>
      <div className="mictest-body">
        <p className="mictest-title">试麦</p>
        <p className="mictest-status" role="status" aria-live="polite">
          {status}
        </p>
        {listening ? (
          <span
            className="levelbar mictest-meter"
            role="meter"
            aria-label={`麦克风电平 ${pct}%`}
            aria-valuenow={pct}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <span
              className={pct > 90 ? "levelfill hot" : "levelfill"}
              style={{ transform: `scaleX(${pct / 100})` }}
            />
          </span>
        ) : null}
        <button
          type="button"
          className={listening ? "btn btn-danger" : "btn btn-primary"}
          onClick={() => void toggle()}
          disabled={busy}
          aria-pressed={listening}
        >
          {listening ? "停止试麦" : "开始试麦"}
        </button>
        {listening ? (
          <p className="hint">戴耳机再试，否则扬声器会回灌啸叫。</p>
        ) : null}
        {err ? (
          <p className="hint" role="alert">
            {err}
          </p>
        ) : null}
      </div>
    </section>
  );
}
