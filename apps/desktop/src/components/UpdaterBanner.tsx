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
    if (import.meta.env.DEV) console.debug("[updater]", error);
    // 生产环境也给一句可操作的提示 + 重试，别点了没反应
    return (
      <div className="updater updater-error" role="alert" aria-live="polite">
        <span className="updater-dot" aria-hidden />
        <span className="updater-text">更新失败：{shortError(error)}</span>
        <span className="updater-actions">
          <button className="btn btn-sm" onClick={() => void updater.checkForUpdate()}>
            重试
          </button>
          <button className="btn btn-sm" onClick={updater.dismiss}>
            关闭
          </button>
        </span>
      </div>
    );
  }

  return null;
}

/** Rust/网络原始报错太长，截一句人话能看的。 */
function shortError(raw: string): string {
  const oneLine = raw.replace(/\s+/g, " ").trim();
  if (/404|not found/i.test(oneLine)) return "更新包地址失效（404），稍后重试。";
  if (/signature|verify/i.test(oneLine)) return "签名校验失败，安装包可能不完整，稍后重试。";
  if (/network|connect|timeout|dns|ssl/i.test(oneLine)) return "网络不通，检查网络后重试。";
  return oneLine.length > 60 ? `${oneLine.slice(0, 60)}…` : oneLine;
}
