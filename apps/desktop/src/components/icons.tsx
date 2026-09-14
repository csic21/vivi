import type { CSSProperties } from "react";
import { levelToPct } from "../hooks/useMicLevel";

function cx(...xs: Array<string | false | null | undefined>) {
  return xs.filter(Boolean).join(" ");
}

/** 话筒：说话时胶囊点亮，试麦时外圈跟着电平呼吸。 */
export function MicIcon({
  lit = false,
  muted = false,
  level = 0,
  size = 22,
}: {
  lit?: boolean;
  muted?: boolean;
  level?: number;
  size?: number;
}) {
  const glow = levelToPct(level) / 100;
  return (
    <span
      className={cx("mic", lit && "lit", muted && "muted")}
      style={
        {
          width: size,
          height: size,
          ["--lvl"]: String(glow),
        } as CSSProperties
      }
      aria-hidden="true"
    >
      <span className="mic-glow" />
      <svg
        viewBox="0 0 24 24"
        width={size}
        height={size}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <rect
          x="9"
          y="2"
          width="6"
          height="11"
          rx="3"
          fill={lit && !muted ? "currentColor" : "none"}
        />
        <path d="M5 11a7 7 0 0 0 14 0" />
        <path d="M12 18v3" />
        <path d="M8 21h8" />
        {muted ? <path d="M4 4l16 16" /> : null}
      </svg>
    </span>
  );
}

export function HeadphoneIcon({ off = false }: { off?: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="1em"
      height="1em"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M4 14v-2a8 8 0 0 1 16 0v2" />
      <rect x="2" y="13" width="5" height="7" rx="1.5" />
      <rect x="17" y="13" width="5" height="7" rx="1.5" />
      {off ? <path d="M4 4l16 16" /> : null}
    </svg>
  );
}

export function GearIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="1em"
      height="1em"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3" />
      <path d="M2 14h4M10 8h4M18 16h4" />
    </svg>
  );
}

export function LeaveIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="1em"
      height="1em"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M9 21H6a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3" />
      <path d="M16 17l5-5-5-5" />
      <path d="M21 12H9" />
    </svg>
  );
}

export function CopyIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      width="1em"
      height="1em"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </svg>
  );
}
