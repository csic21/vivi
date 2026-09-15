import { useCallback, useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export type UpdaterStatus =
  | "idle"
  | "checking"
  | "up-to-date"
  | "available"
  | "downloading"
  | "ready"
  | "error";

export interface UpdaterState {
  status: UpdaterStatus;
  currentVersion: string;
  availableVersion: string | null;
  error: string | null;
  checkForUpdate: () => Promise<void>;
  downloadAndInstall: () => Promise<void>;
  dismiss: () => void;
}

function inTauri(): boolean {
  // tauri dev 下 protocol 是 http，只有 release 包才是 tauri://，
  // 所以只认 __TAURI_INTERNALS__；浏览器直开时没有这个标记，照样跳过。
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * 启动后自动检查一次更新（仅 Tauri 内）。
 * dialog: false，由 UpdaterBanner 接管 UI；下载完用户点“重启更新”。
 */
export function useUpdater(autoCheck = true): UpdaterState {
  const [status, setStatus] = useState<UpdaterStatus>("idle");
  const [currentVersion, setCurrentVersion] = useState("");
  const [availableVersion, setAvailableVersion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const updateRef = useRef<Awaited<ReturnType<typeof check>> | null>(null);
  const dismissedRef = useRef(false);

  const checkForUpdate = useCallback(async () => {
    if (!inTauri()) return;
    dismissedRef.current = false;
    setStatus("checking");
    setError(null);
    try {
      const [v, update] = await Promise.all([getVersion(), check()]);
      setCurrentVersion(v);
      if (update) {
        updateRef.current = update;
        setAvailableVersion(update.version);
        setStatus("available");
      } else {
        updateRef.current = null;
        setAvailableVersion(null);
        setStatus("up-to-date");
      }
    } catch (e) {
      // 未配置公钥 / 无网络 / 非 release 构建都走这里，不打扰用户
      setError(String(e));
      setStatus("error");
    }
  }, []);

  const downloadAndInstall = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;
    setStatus("downloading");
    setError(null);
    try {
      await update.downloadAndInstall();
      setStatus("ready");
      await relaunch();
    } catch (e) {
      setError(String(e));
      setStatus("error");
    }
  }, []);

  const dismiss = useCallback(() => {
    dismissedRef.current = true;
    setStatus("up-to-date");
  }, []);

  // 版本号与更新检查解耦：版本号一律尝试读取（设置页展示用），
  // 只有更新检查才要求 Tauri 环境。浏览器预览时 check 静默跳过。
  useEffect(() => {
    getVersion()
      .then((v) => setCurrentVersion(v))
      .catch(() => setCurrentVersion(""));
  }, []);

  useEffect(() => {
    if (autoCheck) void checkForUpdate();
  }, [autoCheck, checkForUpdate]);

  return {
    status,
    currentVersion,
    availableVersion,
    error,
    checkForUpdate,
    downloadAndInstall,
    dismiss,
  };
}
