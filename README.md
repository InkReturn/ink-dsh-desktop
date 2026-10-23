# inkdsh-desktop

inkdsh 是 DeepSeek Harness（DSH）的桌面客户端。当前仓库处于 **Phase 1：最小可用原型**：Tauri 2 外壳拉起本地 `dsh web` sidecar，WebView2 加载官方 Web UI，关闭窗口时回收子进程树。

已包含的基础加固：单实例、Windows Job Object 崩溃兜底、PID 标记启动清扫、sidecar 日志落盘、等待页失败重试、首启运行时装配状态机（来源解析 / 版本基线 / 托管安装 / 重试）。

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

`runtime:stage` 会把本机 Node 与 `@deepseek-ai/dsh` 复制到 `src-tauri/resources/runtime/`，执行一次 `--version` 冒烟，然后由 Tauri 映射到安装资源目录 `runtime/`。应用启动时按下方来源优先级使用该运行时；没有 staged 运行时则继续回退。

普通构建：

```powershell
pnpm build
```

默认生成 NSIS 安装包，不包含离线运行时。

## sidecar 启动细节

来源优先级（每个候选都先过版本基线）：

1. 环境变量 `DSH_BIN` 指向的 `lib/bin.js`（显式覆盖；路径无效直接失败）
2. 项目本地 `<项目根>/node_modules/@deepseek-ai/dsh/lib/bin.js`
3. PATH 中 `dsh` shim 对应的全局 npm / pnpm 安装（用户已有 DSH）
4. 托管运行时 `<应用数据目录>/runtime/`（前两者缺失或版本不足时才装配）
5. 捆绑运行时 `<资源目录>/runtime/`（`node/` + `dsh/lib/bin.js`，离线包）

最低 DSH 版本来自 `src-tauri/resources/manifest.json` 的 `engines.dsh.minimum`。低版本或版本读不到的候选会被跳过，日志记录逐来源原因后继续回退；`DSH_BIN` 是用户显式选择，低版本只告警不切换。

## 资源预载与 BOOT 注入（P2.3）

- `src-tauri/src/preload.rs`：从已选 DSH 的 `dsh-web-frontend/dist` 读取 `index.html` 与 `assets/*`，缓存约 89 个文件 / 5 MB；同时在 index 的 `</head>` 前注入 `window.__INKDSH_PRELOADED_SESSIONS__`（最近 20 个会话元数据）。
- `src-tauri/src/interceptor.rs`：通过 WebView2 `AddWebResourceRequestedFilter` + `WebResourceRequested` 回调，命中缓存时直接返回内存字节；未命中的请求原样交给 DSH 服务。
- 诊断日志：`<应用数据目录>/logs/preload.log`，记录缓存数量、命中路径与字节数；只记录 origin，不记录 token。
- 降级：拦截器安装失败或找不到前端 dist 时只写日志，页面继续走服务端资源，不阻断启动。

## 插件（P2.1 / P2.2）

- `plugins/ui-chat`（`@inkdsh/ui-chat`）：虚拟列表视图。`@tanstack/react-virtual` 只渲染可视区，上下各 5 条缓冲；流式更新经 100ms 批量缓冲；工具卡片懒加载。官方 0.2.1-rc.2 没有 `ctx.uiConversation.registerTarget`，实际等价入口是 `ctx.slots.inject('conversation.view', () => ctx.slots.register(...))`，已按真实接口注册 `inkdsh-chat` 视图。
  - 已知差距：当前只接管 transcript 渲染，复用官方 `chat` target 的节点数据；markdown、工具专属卡片等仍由官方 chat 负责，也尚未禁用官方 chat。功能等价未达成，属于可增量启用的虚拟视图。
- `plugins/session-preload`（`@inkdsh/session-preload`）：`inject: ['sessionProjections', 'sessions']`，启动时读取 `DSH_HOME/storages/session_projcache/sessions/*.json` 的最近 10 个会话，活会话取 `sessionProjections.snapshot`，否则使用磁盘投影行，缓存到内存 `Map<sessionId, ProjectionState>`。
- 验证命令：
  ```powershell
  pnpm plugins:typecheck
  pnpm plugins:test
  ```
- 当前 `lib/` 由 `tsc` 生成纯 ESM，尚未按 DSH `tsdown`/`__ModuleLoader__` 格式打包，也尚未写入 profile 的 `cordis.patch.yml`；运行时装载未验证。

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
- 启动失败时等待页提供“重试”，通过 `sidecar_retry()` 重新走完整的装配与启动流程。
- 第二次启动应用不会新开实例，只唤起现有主窗口。

## 首启运行时装配

托管运行时固定装配到 `<应用数据目录>/runtime/`，布局与离线包一致：

```text
runtime/
├─ node/node.exe
├─ dsh/lib/bin.js
├─ dsh/package.json
└─ runtime.json          # 版本、安装时间、制品摘要（诊断用）
```

状态机：`detect → installing（校验 / 解压 / 启用）→ ready`，失败进入 `failed` 并允许重试；等待页轮询 `runtime_status()` 展示阶段与逐来源错误。

安装来源由 `resources/manifest.json` 的 `core` 描述：

```json
{
  "engines": { "dsh": { "minimum": "0.2.0" } },
  "core": { "url": null, "sha256": null, "fileName": null }
}
```

正式核心包 URL 与 SHA-256 尚未确定，当前 `core.url` 为空，安装器返回 `RUNTIME_ARTIFACT_UNCONFIGURED`（blocked-by-env），不会伪造下载或跳过完整性校验。已实现并测试的部分：目录 fixture 装配、zip 解压、SHA-256 校验、staging + 备份原子切换；HTTPS 下载传输层待正式制品确定后接入。

开发/测试可用的隔离变量（仅调试构建读取）：

- `INKDSH_DATA_DIR`：覆盖应用数据目录，避免写入真实 AppData。
- `INKDSH_RUNTIME_ARTIFACT`：指向运行时目录或 zip 夹具，验证 `installing → ready`。
- `INKDSH_RUNTIME_ARTIFACT_SHA256`：zip 夹具的摘要（zip 必填）。
- `DSH_HOME`：由 DSH 自身读取；测试时指向 `.temp` 隔离目录，绝不改写 `~/.dsh`。

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
├─ plugins/
│  ├─ ui-chat/                   # @inkdsh/ui-chat 虚拟列表客户端插件
│  └─ session-preload/           # @inkdsh/session-preload 启动预折叠 host 插件
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
   │  ├─ manifest.json          # 最低 DSH 版本 + 核心包制品描述
   │  └─ runtime/
   │     └─ README.md           # runtime/node/ 与 runtime/dsh/ 为可选产物
   └─ src/
      ├─ main.rs                # 窗口、导航、Tauri 入口、数据目录解析
      ├─ sidecar.rs             # 启动/状态/回收，串接运行时装配
      ├─ preload.rs             # 前端 dist/会话缓存 + BOOT 注入
      ├─ interceptor.rs         # WebView2 静态资源内存响应
      └─ runtime/
         ├─ mod.rs              # 错误类型、装配状态机、状态快照
         ├─ version.rs          # semver 解析与最低版本判定
         ├─ manifest.rs         # resources/manifest.json 加载
         ├─ source.rs           # 来源优先级、版本校验、路径探测
         └─ install.rs          # 托管安装、SHA-256、zip、原子切换
```

## 命令

| 命令 | 说明 |
|---|---|
| `sidecar_status()` | 返回 `starting` / `ready` / `failed` / `stopped` 状态快照 |
| `sidecar_url()` | 返回带 token 的官方 Web UI 地址，未就绪时为 `null` |
| `runtime_status()` | 返回 `detect` / `installing` / `ready` / `failed` 装配快照 |
| `sidecar_retry()` | 在失败/停止状态下重新装配并拉起 sidecar，复用当前等待页导航 |

## Phase 1 边界

- 只支持本地 host；多 host、SSH、云隧道属于后续阶段。
- P2.3 已实现静态资源内存响应与 BOOT 注入；P2.1 仅提供可增量启用的虚拟 transcript 视图，未达到官方 chat 功能等价；P2.2 尚未挂入 profile 验证运行时装载。
- 安装包使用 NSIS；默认不内置运行时，`pnpm build:offline` 才生成含 Node + DSH 的离线包。
- 托管运行时的 HTTPS 下载传输层尚未接入：正式核心包地址/摘要确定前，首启装配只能依赖本地/捆绑/全局来源，或开发 fixture。
- 官方 UI 的模型调用依赖本机 DSH 凭据与账号额度；客户端只负责启动和承载页面。

## 许可证

[MIT](LICENSE) © 2026 InkReturn
