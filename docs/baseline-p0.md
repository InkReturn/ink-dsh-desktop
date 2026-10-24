# P0 基线确认（inkdsh 魔改）

- 记录日期：2026-10-24
- 分支：`feat/inkdsh-mods`（从 `47f03f4` 切出，未改一行代码）
- 结论：**fork 能编译、能启动、能跑到 DSH 界面**。基线可用，可以进 P-1/P2。

## 1. 机器与工具链（实测输出）

| 项 | 要求 | 实测 | 结论 |
|---|---|---|---|
| Node | ≥ 24.2 | 系统 `node -v` → `v22.17.1`；`fnm` 内 `v24.16.0` | 系统 Node 不合格，**必须显式走 fnm 24.16.0** |
| pnpm | 11.7.0（根 `packageManager`） | 系统 `10.14.0`；`corepack prepare pnpm@11.7.0 --activate` 后 `11.7.0` | 已按 11.7.0 跑通 |
| Rust | stable + MSVC | `rustc 1.95.0` / `cargo 1.95.0` | 合格（与基线记录一致） |
| git | — | `2.49.0.windows.1` | 合格 |
| WebView2 | 155.0.4283.62 | `155.0.4283.62` | 与基线记录一致 |
| JDK / Android | 可缺 | `openjdk 25.0.1` + Android `Sdk/` | 合格（当前不投入移动端） |

`dsh -V` 的 Node 版本坑已复现并验证：

```
node -v                        → v22.17.1
dsh -V                         → （静默，无输出、无报错）
fnm 24.16.0 + dsh -V           → 0.2.1-rc.2
```

> 影响：sidecar/CLI 相关命令一律用 Node 24 的绝对路径，不靠系统 `node`。

本次执行使用的环境变量（后续每个命令都带）：

```bash
export PATH="/c/Users/inkre/AppData/Roaming/fnm/node-versions/v24.16.0/installation:$PATH"
# 该目录自带 corepack 已激活的 pnpm 11.7.0
```

## 2. 仓库与基线

| 项 | 值 |
|---|---|
| 基线 commit | `47f03f41` `fix(pet): Linux 下用输入形状取代整窗穿透…(#1231)` |
| `origin/main` HEAD | `47f03f41`（与期望一致，无需重新登记基线） |
| 工作分支 | `feat/inkdsh-mods`（已从基线点切出） |
| 子模块 | 14 个 `source/*` 均未初始化（`pnpm install` / 构建不需要；官方核心源码可另取，见 §5） |

## 3. 依赖安装

```
Scope: all 17 workspace projects
Packages: +1550（reused 1547，downloaded 0）
Done in 12.9s using pnpm v11.7.0
```

无 `warn` / `error` / `failed` 行。

## 4. 启动

`package.json` 里 `dev` 只是 `vite`，真正要跑桌面壳是 `dev:desktop`：

```
"dev": "vite",
"dev:desktop": "pnpm dev:plugins & tauri dev"
```

本次观测：`pnpm dev:desktop`（首次，含 580 个 crate 的 Rust 全量编译）。

| 观测点 | 实测 |
|---|---|
| 首次 Rust 编译（t0 → 应用进程起） | ≈ 119 s（580 crates，全新 target 目录） |
| Vite ready | `1318 ms` |
| 进程启动 → 日志就绪 | `STARTUP_BOOT: name=logger_ready elapsed_ms=3` |
| 进程启动 → context 就绪 | `STARTUP_BOOT: name=context_ready elapsed_ms=1088` |
| 壳 UI 首帧可见 | 进程起后 ≈ 9 s（`[Iframe] branch=local-frame iframeLoaded=true`） |
| 首启引导页 | **停住等人工**（见下） |
| 跳过引导后 → sidecar 首响应 | `STARTUP_HTTP: spawn_to_first_response_ms=7749` |
| 跳过引导后 → 完全 ready | `STARTUP_READY: client modules 82/82 ready, spawn_to_ready_ms=8063` |
| sidecar 落点 | `dsh web: http://127.0.0.1:3081/?token=…`（LISTENING，独立 pid） |

**首启会停在「安装推荐插件」引导页**（`preinstall.title`），勾选/确定或「跳过」后才继续
`launchAndWait()`。这是上游设计，不是故障。跳过路径 = `invoke('skip_preinstall_plugins')`。

> 验收口径提醒：`MOD_PLAN §6` 的「冷启动 ≤5s」在 dev 构建下不成立（debug 二进制 + vite dev
> server + 首次编译）。要按 ≤5s 验收，应使用 release 构建并只计「进程启动 → STARTUP_READY」。

## 5. 数据落点（profile / 数据目录）

| 内容 | 路径 | 依据 |
|---|---|---|
| DSH 用户数据根（debug） | `C:\Users\inkre\.dsh.dev` | `config::constants::DSH_HOME_DEV_DIR_NAME`，`debug_assertions` 分支 |
| DSH 用户数据根（release） | `%USERPROFILE%\.dsh`（可用 `DSH_HOME` 覆盖） | `DSH_HOME_DIR_NAME` |
| profile | `~/.dsh.dev/profiles/{web,tauri}`（各带 `package.json` / `cordis.patch.yml` / `node_modules`） | 运行产物 |
| 桌面壳应用数据 | `%APPDATA%\dsh-tauri\dev\`（`logs/`、`dependencies.json`、`updates/`） | 运行产物 |
| 壳日志 | `%APPDATA%\dsh-tauri\dev\logs\desktop.log`、`desktop.frontdesk.log` | 运行产物 |
| WebView2 用户数据 | `%LOCALAPPDATA%\dsh-tauri\EBWebView-dev`（release 为 `EBWebView`） | `desktop/builder.rs::webview_data_directory` |

`dependencies.json` 当前全部为 `<system>`（node / dsh / pnpm / git 都复用系统安装）：

```json
{ "dsh": null, "git": null, "node": null, "pnpm": null }
```

## 6. 报错与告警

无 ERROR。全部 WARN：

| 告警 | 判读 |
|---|---|
| `Runtime files already present although store says not installed, healing installed flag` | 自愈逻辑按预期工作（机器上已有旧 runtime 文件），无害 |
| `[remote] ssh api unreachable: … /api/tauri/remote/machines` | sidecar 未起时的正常探测失败 |
| `Failed to fetch dsh pkg releases …, falling back to git tags` | 版本检查回落，无害 |
| `Suppressing dsh update toast because latest is above recommended (preview/alpha): 0.2.1-rc.3` | 上游推荐版仍是 rc.2，符合预期 |

## 7. 风险与未决

1. **机器上并行跑着旧产物**：`D:\harness\Harenss\极简思维Harness.exe`（今早 08:30 启动，
   旧 `feat-runtime-bootstrap` 产物）。它用 `~/.dsh`，本次 dev 用 `~/.dsh.dev`，目前未冲突；
   但两者都会做 pnpm 插件安装/更新，**同时操作可能抢 pnpm 锁**。建议压测前先关掉。
2. 首启引导必须人工点一次；脚本化复现建议预先写 `preinstall` 完成标记，避免每轮卡住。
3. 冷启动 ≤5s 的验收需改用 release 构建（见 §4 注）。
4. 官方核心源码（`dsh-client-ui-chat` 的 TS）未随仓库带下来；P-1 用 `npm pack` 取
   `0.2.1-rc.2` 的发布物做锚点核对（`lib/client.js` md5 与机器上已安装副本一致，
   均为 `c0343309506321741ac1ca68cc39b86b`，说明该副本未经任何补丁污染）。
