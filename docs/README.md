# docs
- `PLAN.md`：原始 21 节开发计划（单一事实来源）。
- `NETWORK.md`：跨机/公网联调（信令拓扑、STUN、TURN、已知限制）。
- 后续补充：`AUDIO.md`（设备/延迟实测）、`BENCH.md`（各阶段 benchmark）。

备注：`apps/desktop/src-tauri/icons/icon.png` 当前为纯色占位图（满足 `generate_context!` 构建），
正式发版前用 `pnpm --dir apps/desktop tauri icon <1024x1024源图>` 重新生成全套图标。
