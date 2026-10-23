# 可选离线运行时

这个目录用于可选的“离线运行时”构建：

```text
runtime/
├─ node/
│  └─ node.exe
├─ dsh/
│  ├─ lib/bin.js
│  └─ node_modules/...
└─ runtime.json
```

默认构建不包含 Node + DSH。应用按以下来源顺序启动（每个候选都先过最低版本校验）：

1. `DSH_BIN` 显式覆盖（路径无效直接失败，不回退）
2. 项目本地 `node_modules/@deepseek-ai/dsh`
3. 全局 `dsh`（用户已有安装）
4. 托管运行时 `<应用数据目录>/runtime/`（前两者缺失或版本不足时才装配）
5. 捆绑运行时（本目录，离线包）

最低 DSH 版本由 `src-tauri/resources/manifest.json` 的 `engines.dsh.minimum` 声明。低版本或读不到版本的候选会被跳过，并把原因写入 sidecar 日志。

若需要安装包自带运行时，先执行：

```powershell
pnpm runtime:stage
pnpm build:offline
```

`runtime:stage` 会把本机 Node 和 `@deepseek-ai/dsh` 复制到本目录，Tauri 打包时映射到安装资源目录 `runtime/`。

不要把 `node/`、`dsh/` 或 `runtime.json` 提交到 Git，它们已被 `.gitignore` 排除；本 README 与 `resources/manifest.json` 需要提交。

托管运行时的正式核心包 URL 与 SHA-256 尚未确定，`manifest.json` 中保持 `null`；此时安装器返回 blocked-by-env，不会伪造下载或跳过完整性校验。
