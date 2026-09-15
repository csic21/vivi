import { useEffect, useRef, useState } from "react";
import { useVoiceStore } from "../stores/useVoiceStore";
import { levelIsHot, levelToPct, useMicLevel } from "../hooks/useMicLevel";

/**
 * 音频检查：紧凑的状态条（状态点 + 电平 + 操作），代替原来的大圆形试麦卡。
 * compact 用在房间设置里。
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

  const hot = listening && levelIsHot(level);
  const dot = stale || !listening ? "" : hot ? "ok" : "waiting";
  const status = stale
    ? "电平无数据：重启应用"
    : !listening
      ? "检查麦克风是否正常工作"
      : hot
        ? "输入正常"
        : "正在监听，请说话";

  return (
    <section
      className={compact ? "audiocheck compact" : "audiocheck"}
      aria-label="音频检查"
    >
      <span className={dot ? `ac-dot ${dot}` : "ac-dot"} aria-hidden="true" />
      <div className="ac-main">
        <p className="ac-title">音频检查</p>
        <p className="ac-status" role="status" aria-live="polite">
          {status}
        </p>
        {listening ? (
          <span
            className="levelbar ac-meter"
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
        {listening ? <p className="hint">建议佩戴耳机，避免扬声器回灌。</p> : null}
        {err ? (
          <p className="hint" role="alert">
            {err}
          </p>
        ) : null}
      </div>
      <button
        type="button"
        className={listening ? "btn" : "btn btn-primary"}
        onClick={() => void toggle()}
        disabled={busy}
        aria-pressed={listening}
      >
        {listening ? "停止" : "试麦"}
      </button>
    </section>
  );
}
