import { useVoiceStore } from "../stores/useVoiceStore";

/** 右上角链路 pill 簇：取全场最差值，有一路中继就标中继。 */
export function StatsBar() {
  const members = useVoiceStore((s) => s.members);
  if (members.length === 0) return <div className="pills"><span className="pill"><span className="k">链路</span>等待队友</span></div>;
  const rtts = members
    .map((m) => m.rtt_ms)
    .filter((v): v is number => v !== null);
  const worstRtt = rtts.length ? Math.round(Math.max(...rtts)) : null;
  const worstJitter = Math.round(Math.max(...members.map((m) => m.jitter_ms)));
  const worstLoss = Math.max(...members.map((m) => m.loss_percent));
  const relayed = members.some((m) => m.route === "Relay");
  const bad = worstLoss > 5 || (worstRtt !== null && worstRtt > 300);
  return (
    <div className="pills" aria-live="polite">
      <span className="pill">
        <span className="k">延迟</span>
        {worstRtt ?? "—"}ms
      </span>
      <span className="pill">
        <span className="k">抖动</span>
        {worstJitter}ms
      </span>
      <span className={worstLoss > 5 ? "pill pill-bad" : "pill"}>
        <span className="k">丢包</span>
        {worstLoss.toFixed(1)}%
      </span>
      <span className={bad ? "pill pill-bad" : relayed ? "pill pill-warn" : "pill pill-ok"}>
        {bad ? "网络差" : relayed ? "中继" : "直连"}
      </span>
    </div>
  );
}
