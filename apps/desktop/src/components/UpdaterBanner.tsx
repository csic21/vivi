import type { UpdaterState } from "../hooks/useUpdater";

/** 非阻塞顶部横幅：有新版本才出现，下载完自动重启。 */
export function UpdaterBanner({ updater }: { updater: UpdaterState }) {
  const { status, availableVersion, currentVersion, error } = updater;

  if (status === "available" || status === "downloading" || status === "ready") {
    return (
      <div className="updater" role="status" aria-live="polite">
        <span className="updater-dot" aria-hidden />
        <span className="updater-text">
          {status === "available" &&
            `发现新版本 v${availableVersion}（当前 v${currentVersion}），建议更新。`}
          {status === "downloading" && `正在下载 v${availableVersion}…`}
          {status === "ready" && "更新就绪，正在重启…"}
        </span>
        {status === "available" && (
          <span className="updater-actions">
            <button
              className="btn btn-primary btn-sm"
              onClick={() => void updater.downloadAndInstall()}
            >
              更新并重启
            </button>
            <button className="btn btn-sm" onClick={updater.dismiss}>
              稍后
            </button>
          </span>
        )}
      </div>
    );
  }

  if (status === "error" && error) {
    // 仅控制台留痕：公钥未配/离线时不打扰（首次发版前常见）
    if (import.meta.env.DEV) console.debug("[updater]", error);
    return null;
  }

  return null;
}
