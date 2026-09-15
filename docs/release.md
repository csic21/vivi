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

顺序上 `latest.json` 一定先上传、Release 后转正，然后流水线最后一步会跑
`verify-latest-json.mjs` 做公网自检：等 `releases/latest` 翻到本次 tag，
断言每个实际构建的平台族都有 key、每个产物的 URL 都下得动。这一步不过
就是发版失败，别手动绕过。

验证：打开刚发布的 Release，应看到各平台安装包、
对应的 `.sig` 文件和 `latest.json`；浏览器访问
`https://github.com/csic21/vivi/releases/latest/download/latest.json`
应返回带各平台（或实际构建出的平台）的 JSON。

## 发信令服务器新版（可选，独立版本线）

```bash
git tag signaling-v0.2.0 && git push origin signaling-v0.2.0
```

产物为 `signaling-signaling-v0.2.0-<target>.tar.gz/.zip`，
下载后直接跑 `./signaling` 即可（桌面端自带内嵌信令，一般不需要单独部署）。

## 常见问题

- **客户端没提示更新**：先看一眼 endpoint 到底返回了什么：
  ```bash
  curl -sL https://github.com/csic21/vivi/releases/latest/download/latest.json \
    | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const m=JSON.parse(s);console.log(m.version, Object.keys(m.platforms).join(", "))})'
  ```
  三个坑按概率排：
  1. **`platforms` 里缺你那个平台的 key**。客户端按 `{os}-{arch}-{installer}`
     → `{os}-{arch}` 找 key，一个都没有就抛 `TargetsNotFound`，
     界面表现得和"已是最新"一模一样。2026-09-15 的 v0.3.0/v0.3.1 就是这么翻车的
     —— 脚本不认识裸 `.exe`/`.msi`，整个 windows 平台被静默跳过。
     现在 `gen-latest-json.mjs` 遇到认不出的 `.sig` 会直接报错，CI 不会再放过。
  2. **`version` 没跟着 tag 走**：Release 还是草稿时 `releases/latest` 会指向上一个版本，
     客户端自然"已是最新"。转正后再等 `releases/latest` 翻转（流水线里的自检就在等这个）。
  3. **`pubkey` 没替换**（占位符会导致签名校验失败）。
- **点了"更新并重启"没反应**：以前这种情况是设计成的——`status === "error"` 时横幅直接
  不渲染，所以下载失败/签名失败/安装失败看起来都像"按钮点不动"。现在横幅会显示一行
  人话报错 + 重试，原始报错在设置页的版本区。排查时先看那里。
- **macOS 提示“已损坏，无法打开”或“无法验证开发者”**：
  不是安装包坏了。当前没有 Apple 公证，Chrome/Safari 下载后会被 Gatekeeper 隔离；
  Sequoia 把未公证的包显示成「已损坏」。把 App 拖进「应用程序」后在终端执行：
  ```bash
   codesign --force --deep --sign - /Applications/Vivi.app
   xattr -cr /Applications/Vivi.app
   open /Applications/Vivi.app
  ```
  以后要双击直接开，需要 Apple Developer 账号做签名+公证。
- **Intel Mac 用户**：**跑不了**。当前只出 `macos-latest`（Apple Silicon）的
  arm64 包，清单里也只有 `darwin-aarch64` 一个 mac key。Rosetta 2 是让 M 系列
  跑 x86 程序用的，反过来不成立。要支持 Intel 得加一个 Intel runner 的构建位
  （`macos-13` 或更新的 Intel 镜像，注意 GitHub 会陆续下线 Intel runner），
  并在清单里补出 `darwin-x86_64`。
- **想先验证流水线不发版**：Actions 页手动 `workflow_dispatch`
  跑一次（只构建，产物不进 Release）。
