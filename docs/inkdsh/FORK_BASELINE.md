# inkdsh 上游基线记录（Fork Baseline）

> 永远致敬上游。本文件记录我们 fork 的出处与基线点，新机器/新会话接手时必须先读。
> 上游自己的同步治理规范见仓库 `docs/specs/upstram.sync.md`（铁律：先汇报、后实施）。

## 上游项目

| 项 | 值 |
|---|---|
| 上游仓库 | `dsh-tauri/deepseek-harness-desktop`（DeepSeek Harness Tauri 桌面版） |
| 上游许可证 | MIT（另见 `LICENSE.details`、`THIRD_PARTY_NOTICES.md`） |
| 上游 README 声明 | 推荐 DSH `0.2.1-rc.2` |
| 上游规模（基线点） | 2,126 文件、TS/TSX 1,447、Rust 217、代码约 24.5 万行、16 个 packages |

## 我们的 fork

| 项 | 值 |
|---|---|
| fork 仓库 | `InkReturn/ink-dsh-desktop`（GitHub fork 关系已确认，API `fork: true`） |
| fork 创建时间 | 2026-10-23 19:28:50 UTC |
| 上游基线 commit | `47f03f4117bb3a8a566fddb61c4a9fc11646e904`（2026-10-23 22:56:15 +0800，`fix(pet): Linux 下用输入形状取代整窗穿透… (#1231)`） |
| fork main 冻结点 | 同上 `47f03f4`，记为 **基线 v0.25.0** |
| 工作分支 | `feat/inkdsh-mods`（从 fork main 基线点切出，承载全部魔改） |
| 参考分支 | `feat-runtime-bootstrap`（独立无父历史，旧 inkdsh 自研成果，`aa3e283`，仅作移植来源，不再继续开发） |

## 分支约定

| 分支 | 用途 | 禁止事项 |
|---|---|---|
| `main` | 与上游同步的净分支，只跑 upstream sync | 不放自己的功能改动 |
| `feat/inkdsh-mods` | 魔改主分支，所有自研改动都在这里 | 不直接推，只走合并 |
| `feat/p*-<主题>` | 每阶段一个短分支，合回 `feat/inkdsh-mods` | 不长期存活 |
| `feat-runtime-bootstrap` | 只读参考，移植完成后打 tag 封存 | 不再提交新代码 |

## 上游功能盘点（基线 v0.25.0，已核实）

| 能力 | 位置 | 状态 | 我们还要不要做 |
|---|---|---|---|
| 拉起 `dsh web` sidecar（首启安装核心、端口选择、进程回收、Windows Job Object、启动耗时观测） | `src-tauri/src/service/workflow/`、`desktop/startup.rs` | ✅ 已有，比我们写的更全 | 不做，直接用 |
| 静态资源预载 + WebView2 请求拦截 | `src-tauri/src/desktop/frontend_preload.rs`（Windows `WebResourceRequested` 过滤） | ✅ 已有 | 验收，不重复造 |
| 官方核心文件补丁体系（16 个补丁模块，launch 时逐个注入，best-effort 失败告警） | `src-tauri/src/service/patch/*.rs` + `workflow/launch.rs` 挂点 | ✅ 已有 | **我们的性能魔改按这个风格追加** |
| chat 性能补丁（去 `:has()`、滚动跟随镜像，issue #1187） | `service/patch/chat_follow_anchor.rs`、`scroll_follow.rs` | ✅ 已有 | 重叠，直接采用；我们只补它没做的 |
| 多 Host：远程机器管理、SSH 传输、隧道（cloudflared）、配对、网关、访问区段 UI | `packages/dsh-tauri-remote/` | ✅ 已有 | 验收 + 按需要调形态 |
| Host 插件体系：`plugin.host.md` / `plugin.host.service.md` / `plugin.client*.md` | `docs/specs/` | ✅ 有规格文档 | 照读照用 |
| profile 管理（桌面端独立 `desktop` profile、核心 bundle 自愈） | `service/profile/mod.rs` | ✅ 已有 | 研究其隔离模型，复用 |
| 数据目录管理（迁移、备份、切换） | `service/data_dir/`、`bridge/backup.rs` | ✅ 已有 | 复用其快照思路 |
| 安全恢复（启动失败的恢复页） | `desktop/builder.rs` recovery 分支 | ✅ 已有 | 验收 |
| 移动端（React Native） | `src-native/`、`packages/dsh-tauri-mobile-ui` | 保留（用户决策 4：留着） | 当前不投入 |
| 桌宠 | `service/desktop/pet*.rs`、`packages/dsh-tauri-pet` | 保留（决策 4） | 不投入 |
| 通知 / 调度 / 右键菜单 / worktree / archive / experimental / extension | `packages/dsh-tauri-*` | 保留（决策 4） | 不投入 |
| **我们要加的：流式 100ms 批量渲染** | 无 | ❌ 缺 | **新增 patch 模块** |
| **我们要加的：`content-visibility` 消息级渲染跳过** | 无 | ❌ 缺 | **新增 patch 模块** |
| **我们要加的：会话投影预折叠（session-preload）** | 无 | ❌ 缺 | **新增 patch 或 host 能力** |
| 皮肤兼容（aqua / liquid-glass 等） | 无 | 用户决策 3：暂不做，后面重搓皮肤 | 搁置 |

## 移植来源（feat-runtime-bootstrap @ aa3e283，只读）

| 要移植的 | 来源文件 | 落地位置 | 方式 |
|---|---|---|---|
| BatchBuffer（流式 entries 100ms 批量提交） | 旧 `plugins/ui-chat/src/official/client/chat/*` 的定制 diff | 新 `service/patch/chat_stream_batch.rs` | 对照旧 diff 手写补丁，新版本对齐 |
| `content-visibility` 渲染跳过 | 同上 | 新 `service/patch/chat_content_visibility.rs` | 同上（需先在目标 WebView2 验证，见 P-1 探针） |
| `:has()` → `data-compaction-has-body` 属性选择器 | 旧 diff | **已存在**：上游 `chat_follow_anchor.rs` 已含等价补丁 | 不重做，验收即可 |
| `window.setTimeout` → `setTimeout` | 旧 diff | 视补丁需要合入对应模块 | 小改动随补丁带 |
| session-preload 预折叠思路 | 旧 `plugins/session-preload/src/preload.ts` | 新 host 能力（挂在 profile/launch 流程） | JavaScript 侧 patch 或核心 patch，P2 评估选型 |
| esbuild 客户端 bundle 构建脚本 | 旧 `plugins/ui-chat/scripts/build-client.mjs` | 若后续需要自研 client 插件再拿 | 暂不移植 |

## 版本与运行环境钉死

| 项 | 钉死值 | 依据 |
|---|---|---|
| Node | **≥24.2**（开发机 fnm 已装 v24.16.0；sidecar 必须显式绑 Node 24 可执行路径，禁止依赖系统 `node`——实测 Node 22 上 `dsh -V` 静默无输出） | 新机器实测，`import.meta.main` |
| pnpm | `11.7.0`（根 packageManager 声明） | fork main `package.json` |
| DSH 内核 | `0.2.1-rc.2` | 上游徽章 + 旧方案一致 |
| Rust | stable + MSVC（实测 1.95.0） | 新机器实测 |
| WebView2 | 155.0.4283.62 | 新机器实测 |
| JDK/Android | 可缺（mobile 保留但不投入） | 决策 4 |

> 更新规则：上游 sync 时必须先在此文件登记新基线 commit，再动手； patch 模块逐个记录其针对的核心包路径与补丁锚点（`ORIGINAL_*` / `PATCHED_*` 常量），官方升级后逐锚点核对。
