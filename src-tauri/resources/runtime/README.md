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

默认构建不包含 Node + DSH，此时应用按以下顺序回退：

1. `DSH_BIN` 显式覆盖
2. 项目本地 `node_modules/@deepseek-ai/dsh`
3. 全局 `dsh`

若需要安装包自带运行时，先执行：

```powershell
pnpm runtime:stage
pnpm build:offline
```

`runtime:stage` 会把本机 Node 和 `@deepseek-ai/dsh` 复制到本目录，Tauri 打包时映射到安装资源目录 `runtime/`。应用启动会优先使用该运行时。

不要把 `node/`、`dsh/` 或 `runtime.json` 提交到 Git，它们已被 `.gitignore` 排除。
