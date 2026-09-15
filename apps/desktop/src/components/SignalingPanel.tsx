import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import {
  clearSignalingOverride,
  getSignalingHttp,
  getSignalingMode,
  setSignalingHttp,
  subscribeSignaling,
  wsFromHttp,
} from "../config";
import { DISCOVERY_HINT } from "../api/signaling";
import { ipc } from "../ipc";
import type { SignalingStatus } from "../types";

/** 连接状态 + 高级连接设置。
 *
 * 主界面只留一行状态条：新手不该被"信令地址"这个词绊住，默认全自动。
 * 地址框降级进「高级」，只有自动发现不灵（跨网络、AP 隔离、公司网）时才需要动。
 */
export function SignalingPanel({
  busy,
  onError,
}: {
  busy: boolean;
  onError: (msg: string | null) => void;
}) {
  const mode = useSyncExternalStore(subscribeSignaling, getSignalingMode, getSignalingMode);
  const current = useSyncExternalStore(subscribeSignaling, getSignalingHttp, getSignalingHttp);

  const [status, setStatus] = useState<SignalingStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState("");
  const [saved, setSaved] = useState(false);
  const [found, setFound] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await ipc.getSignalingStatus());
    } catch {
      // 非 Tauri 预览环境没有这个 command
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 展开时才把当前地址填进输入框，避免用户正在输入时被状态刷新覆盖
  useEffect(() => {
    if (open) setDraft(current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const signalingDown = status !== null && status.port === null;

  let tone: "ok" | "warn" | "manual" = "ok";
  let label: string;
  if (signalingDown) {
    tone = "warn";
    label = "本机信令没起来（8080–8089 都被占用）";
  } else if (mode === "manual") {
    tone = "manual";
    label = `手动指定：${current}`;
  } else if (mode === "auto") {
    label = `自动 · 上次连的是 ${current}`;
  } else {
    label = `自动（本机 ${current}）`;
  }

  const save = async () => {
    const raw = draft.trim();
    if (!raw) {
      onError("地址不能为空。想回到自动模式就点「改回自动」。");
      return;
    }
    if (!/^https?:\/\//.test(raw)) {
      onError("地址要以 http:// 或 https:// 开头，例如 http://192.168.1.10:8080。");
      return;
    }
    const http = setSignalingHttp(raw);
    setDraft(http);
    onError(null);
    try {
      await ipc.setSignalingUrl(wsFromHttp(http));
    } catch {
      /* 非 Tauri 预览环境忽略 */
    }
    setSaved(true);
    setTimeout(() => setSaved(false), 1500);
  };

  const backToAuto = () => {
    clearSignalingOverride();
    onError(null);
    setFound(null);
  };

  const rediscover = async () => {
    setWorking(true);
    setFound(null);
    try {
      const res = await ipc.discoverSignaling(2500);
      if (res.error) {
        setFound(`自动发现用不了：${res.error}`);
      } else if (res.servers.length === 0) {
        setFound(`没发现任何 Vivi。${DISCOVERY_HINT}`);
      } else {
        setFound(
          res.servers
            .map((s) => `${s.label}（${s.addresses[0] ?? "?"}:${s.port}）`)
            .join("；"),
        );
      }
    } catch (e) {
      setFound(String(e));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="panel signaling-panel">
      <button
        type="button"
        className={`sig-pill sig-pill-${tone}`}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="sig-dot" aria-hidden />
        <span className="sig-label">{label}</span>
        <span className="sig-toggle" aria-hidden>
          {open ? "收起" : "高级"}
        </span>
      </button>

      {open ? (
        <div className="sig-detail">
          <div className="device-row">
            <label htmlFor="sig">地址</label>
            <input
              id="sig"
              className="field"
              autoComplete="off"
              spellCheck={false}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void save();
              }}
              placeholder="http://房主IP:8080"
            />
            <button type="button" className="btn" disabled={busy} onClick={() => void save()}>
              {saved ? "已保存" : "保存"}
            </button>
          </div>

          <div className="sig-actions">
            <button type="button" className="btn btn-sm" disabled={working} onClick={() => void rediscover()}>
              {working ? "发现中…" : "重新发现"}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={mode !== "manual"}
              onClick={backToAuto}
            >
              改回自动
            </button>
          </div>

          {found ? <p className="hint">{found}</p> : null}

          <p className="hint">
            默认全自动：同一个 Wi-Fi 下输房号就能连，跨网络让房主点「邀请」把地址发你。
            只有自动发现被网络挡住（公司/校园网、AP 隔离）时才需要手填——填了就是要两边都指向房主那台。
            本机信令 {status?.local_http ?? "未就绪"}
            {status?.embedded === false ? "（复用了已在运行的进程）" : ""}。
          </p>
        </div>
      ) : null}
    </div>
  );
}
