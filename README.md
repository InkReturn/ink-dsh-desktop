# inkdsh-desktop

inkdsh 是 DeepSeek Harness（DSH）的桌面客户端。当前仓库处于 **Phase 1：最小可用原型**：Tauri 2 外壳拉起本地 `dsh web` sidecar，WebView2 加载官方 Web UI，关闭窗口时回收子进程树。

已包含的基础加固：单实例、Windows Job Object 崩溃兜底、PID 标记启动清扫、sidecar 日志落盘、等待页失败重试。

## 前置条件

- Windows 10/11，已安装 WebView2 Runtime（Windows 11 自带）
- Node.js 20+（实测 Node.js 24）
- pnpm 10+（实测 pnpm 11.21）
- Rust stable + MSVC 工具链
- DSH CLI 二选一：
  - **推荐：全局安装** `npm i -g @deepseek-ai/dsh`（实测 `0.2.1-rc.2`）
  - 或把 `@deepseek-ai/dsh` 安装到本项目，sidecar 会优先使用项目内版本

> 官方 DSH 包会带上约 190 MB 的 LibreOffice Office 运行时。Phase 1 不需要 Office 技能，因此 `pnpm install` 只安装 Tauri 外壳依赖，DSH 推荐走全局安装。若确实需要项目内 DSH，执行 `pnpm add @deepseek-ai/dsh`，sidecar 会自动优先使用它。

## 运行

```powershell
cd <项目根目录>
pnpm install
pnpm tauri dev
```

首次运行会编译 Rust 依赖，耗时取决于本机缓存。窗口先显示 inkdsh 等待页，sidecar 就绪后自动导航到官方 DSH Web UI。

## 可选离线运行时

默认安装包不包含 Node + DSH，适合开发机。需要安装包自带运行时（离线安装）时：

```powershell
pnpm runtime:stage
pnpm build:offline
```

`runtime:stage` 会把本机 Node 与 `@deepseek-ai/dsh` 复制到 `src-tauri/resources/runtime/`，执行一次 `--version` 冒烟，然后由 Tauri 映射到安装资源目录 `runtime/`。应用启动时优先使用该运行时；没有 staged 运行时则回退 `DSH_BIN`、项目本地、全局 DSH。

普通构建：

```powershell
pnpm build
```

默认生成 NSIS 安装包，不包含离线运行时。

## sidecar 启动细节

查找顺序：

1. 环境变量 `DSH_BIN` 指向的 `lib/bin.js`
2. 可选离线运行时 `<资源目录>/runtime/`（`node/` + `dsh/lib/bin.js`）
3. `<项目根>/node_modules/@deepseek-ai/dsh/lib/bin.js`
4. PATH 中 `dsh` shim 对应的全局 npm / pnpm 安装

启动命令等价于：

```powershell
node <dsh-bin.js> web --host 127.0.0.1 --port <portpicker-port> --no-open
```

只监听 `127.0.0.1`。stdout 中的官方就绪行格式为：

```text
dsh web: http://127.0.0.1:<port>/?token=<token>
```

解析成功后窗口导航到该地址。就绪行与 token 均由官方 CLI 打印，本项目按其输出格式解析，sidecar 只负责监控子进程存活与状态。

关闭窗口时，Windows 使用 `taskkill /PID <pid> /T /F` 回收整个 sidecar 进程树；Unix 使用进程组 TERM/KILL。

额外保护：

- Windows Job Object 设置 `KILL_ON_JOB_CLOSE`，应用崩溃或被强杀时系统自动结束 sidecar 进程树。
- 启动时读取 `<应用数据目录>/sidecar.pid`，仅当记录端口仍由记录 PID 监听时回收残留进程。
- 日志写入 `<应用数据目录>/logs/sidecar.log`，token 统一替换为 `<redacted>`，超过 5 MB 轮转为 `sidecar.log.1`。
- 启动失败时等待页提供“重新启动内核”，通过 `sidecar_retry()` 重新走完整启动流程。
- 第二次启动应用不会新开实例，只唤起现有主窗口。

## 目录

```text
inkdsh-desktop/
├─ package.json
├─ pnpm-workspace.yaml
├─ pnpm-lock.yaml
├─ .gitignore
├─ README.md
├─ scripts/
│  └─ stage-runtime.ps1         # 可选离线运行时 staging
├─ src/
│  └─ waiting.html              # 本地等待页 + sidecar 状态轮询
└─ src-tauri/
   ├─ Cargo.toml
   ├─ Cargo.lock
   ├─ build.rs
   ├─ tauri.conf.json
   ├─ capabilities/
   │  └─ main.json
   ├─ icons/
   │  └─ icon.ico
   ├─ resources/
   │  └─ runtime/
   │     └─ README.md           # runtime/node/ 与 runtime/dsh/ 为可选产物
   └─ src/
      ├─ main.rs                # 窗口、导航、Tauri 入口
      └─ sidecar.rs             # 定位、启动、解析、状态、回收、运行时选择
```

## 命令

| 命令 | 说明 |
|---|---|
| `sidecar_status()` | 返回 `starting` / `ready` / `failed` / `stopped` 状态快照 |
| `sidecar_url()` | 返回带 token 的官方 Web UI 地址，未就绪时为 `null` |
| `sidecar_retry()` | 在失败/停止状态下重新拉起 sidecar，并复用当前等待页导航 |

## Phase 1 边界

- 只支持本地 host；多 host、SSH、云隧道属于后续阶段。
- 不做前端资源预载、会话投影预载或 chat 插件替换。
- 安装包使用 NSIS；默认不内置运行时，`pnpm build:offline` 才生成含 Node + DSH 的离线包。
- 官方 UI 的模型调用依赖本机 DSH 凭据与账号额度；客户端只负责启动和承载页面。

## 许可证

[MIT](LICENSE) © 2026 InkReturn
