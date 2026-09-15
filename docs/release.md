# 发版与自动更新

三条流水线（`.github/workflows/`）：

| 流水线 | 触发 | 产物 |
|---|---|---|
| `ci.yml` | push/PR 到 main | 版本一致性 + 三平台 `cargo check`/`clippy` + 前端 `tsc`/`vite build` |
| `release-desktop.yml` | tag `v*.*.*` | Win x64 / macOS ARM64 / Linux x64 安装包 + updater `latest.json` |
| `release-signaling.yml` | tag `signaling-v*.*.*` | 信令服务器三平台二进制 |

更新机制：Tauri updater，客户端启动时检查
`https://github.com/csic21/vivi/releases/latest/download/latest.json`，
有新版本弹横幅 → 用户点“更新并重启”。UI 见
`apps/desktop/src/components/UpdaterBanner.tsx`，
逻辑见 `apps/desktop/src/hooks/useUpdater.ts`。

## 一次性准备（只需做一次）

1. 生成签名密钥（本机跑一次即可）：
   ```bash
   pnpm --dir apps/desktop tauri signer generate -w ~/.tauri/vivi.key
   # 输出：公钥（填到 tauri.conf.json）+ 私钥文件（填到 GitHub Secrets）
   # 询问密码时可直接回车（留空），CI 就不用配 PASSWORD
   ```
2. 把输出的公钥填到 `apps/desktop/src-tauri/tauri.conf.json`
   → `plugins.updater.pubkey`（替换掉 `REPLACE_WITH_UPDATER_PUBLIC_KEY`）。
3. GitHub 仓库 → Settings → Secrets and variables → Actions → New secret：
   - `TAURI_SIGNING_PRIVATE_KEY`：粘贴 `~/.tauri/vivi.key` 私钥文件**全部内容**
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：生成密钥时设的密码（留空则填空字符串也要建一个，或删掉 workflow 里对应行）
4. 私钥文件**不要**提交到仓库，`.gitignore` 已覆盖常见路径，`~/.tauri/` 在仓库外最安全。

## 发桌面端新版

```bash
# 1. 四处版本号改成同一个（CI 会强制检查）
#    Cargo.toml [workspace.package]
#    apps/desktop/package.json
#    apps/desktop/src-tauri/Cargo.toml
#    apps/desktop/src-tauri/tauri.conf.json
node apps/desktop/scripts/check-versions.mjs   # 先自查

# 2. 提交并打 tag（tag 必须与代码版本一致，如代码 0.2.0 则打 v0.2.0）
git add -A && git commit -m "chore: release v0.2.0"
git tag v0.2.0 && git push origin main v0.2.0
```

流水线会自动：三平台构建 → 上传安装包到 draft Release →
生成 `latest.json` → 转正 Release。客户端下次启动即收到更新。

验证：打开刚发布的 Release，应看到各平台安装包、
对应的 `.sig` 文件和 `latest.json`；浏览器访问
`https://github.com/csic21/vivi/releases/latest/download/latest.json`
应返回带四个平台（或实际构建出的平台）的 JSON。

## 发信令服务器新版（可选，独立版本线）

```bash
git tag signaling-v0.2.0 && git push origin signaling-v0.2.0
```

产物为 `signaling-signaling-v0.2.0-<target>.tar.gz/.zip`，
下载后直接跑 `./signaling` 即可（桌面端自带内嵌信令，一般不需要单独部署）。

## 常见问题

- **客户端没提示更新**：先确认 Release 里有 `latest.json`；
  再确认 `pubkey` 已替换（占位符会导致签名校验失败，前端静默跳过，
  DEV 模式下控制台有 `[updater]` 日志）。
- **macOS 提示“已损坏，无法打开”或“无法验证开发者”**：
  不是安装包坏了。当前没有 Apple 公证，Chrome/Safari 下载后会被 Gatekeeper 隔离；
  Sequoia 把未公证的包显示成「已损坏」。把 App 拖进「应用程序」后在终端执行：
  ```bash
   codesign --force --deep --sign - /Applications/Vivi.app
   xattr -cr /Applications/Vivi.app
   open /Applications/Vivi.app
  ```
  以后要双击直接开，需要 Apple Developer 账号做签名+公证。
- **Intel Mac 用户**：当前只打 ARM64 包，Intel 机经 Rosetta 2 可运行；
  有需求再加 `macos-13`（Intel）构建位。
- **想先验证流水线不发版**：Actions 页手动 `workflow_dispatch`
  跑一次（只构建，产物不进 Release）。
