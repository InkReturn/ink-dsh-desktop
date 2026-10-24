> 用法：新机器上开一个会话，把本文件**整段**粘贴给 AI（同目录的 `MOD_PLAN.md`、`FORK_BASELINE.md` 作为附件一起给）。它从环境自检开始，一路走到 P0 基线确认。

---

# 任务：魔改 dsh-tauri fork，做自己的 inkdsh

你是 inkdsh 项目的主开发者。我们在 fork `InkReturn/ink-dsh-desktop`（上游 `dsh-tauri/deepseek-harness-desktop` v0.25.0）上做魔改。

**形态：加法。** 上游的功能（壳、sidecar、补丁体系、多主机、桌宠、移动端……）全部保留；我们只在它的**补丁体系**里追加三个东西（流式批量渲染、content-visibility、会话预折叠），**不重写 UI、不删任何上游包**。

## 0. 四条铁律（违反即返工）

1. **先汇报、后实施**：上游 `docs/specs/upstram.sync.md` 的铁律，在魔改期间同样有效。技术选型、补丁锚点、阶段切换——每个都要先给出验证证据再动手。
2. **一次只做一个阶段**：P0 → P-1 → P2 → P3 → P4，阶段之间有汇报检查点。不许跳。
3. **不许把"应该能跑"当结论**：每条命令贴真实输出；测试 0 用例跑到 = 没验证；改锚点前必须先定位到官方源码原文。
4. **补丁只走 best-effort 通道**：锚点失配 → `log::warn!` 跳过，**绝不允许**一个补丁失配把启动搞挂。

## 1. 环境自检（逐条贴输出）

```powershell
node -v                                    # 必须 ≥24.2，不行就 fnm 切
pnpm -v                                    # 建议 11.7.0（根 packageManager 声明）
rustc -V ; cargo -V
git --version
Get-ChildItem 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application' -Directory | Select-Object Name
```

- **Node 版本是真坑**：上游推荐 DSH `0.2.1-rc.2`，它依赖 `import.meta.main`，Node 22 下 `dsh -V` **静默无输出**不报错。sidecar 的启动命令必须显式用 Node 24 的绝对路径，不靠系统 `node`。
- 任何一项缺失：列出、说明影响、给两个选项（装 / 换方案），**停下来等我回**，不要自作主张。

## 2. 拉仓库 + 切分支

```powershell
git clone https://github.com/InkReturn/ink-dsh-desktop.git
cd ink-dsh-desktop
git log --oneline -1

# 从基线点切魔改主分支
git checkout -b feat/inkdsh-mods 47f03f4117bb3a8a566fddb61c4a9fc11646e904
git log --oneline -3
```

- 预期：main HEAD 是 `47f03f4`。如果不是（上游又推了），先把新 HEAD 报给我登记基线，再决定从哪切。
- **禁止**在 `main` 上直接改代码；**禁止**用旧的 `feat-runtime-bootstrap` 分支作为工作分支——它只用作移植来源。

## 3. 装依赖 + P0 基线确认（0.5 天）

```powershell
pnpm install
pnpm dev
```

记录：
- `pnpm dev` 是否一路走到 DSH 界面可见；
- Cold start 耗时（从命令执行到界面 ready，秒表）；
- 应用把 profile / data dir 放在哪（`%APPDATA%\dsh-tauri\` 之类的实际路径）；
- 控制台/日志有没有启动错误。

**P0 不动一行代码。** 只做观察与记录，产出 `docs/baseline-p0.md`。

## 4. 读文档（按顺序）

1. `FORK_BASELINE.md`（附件）——上游基线与我们 fork 的出处，**必须复述上游 fork 关系与基线 commit**。
2. `MOD_PLAN.md`（附件）——魔改清单、补丁规范、阶段计划。
3. 上游自己写的：`docs/specs/desktop.baisc.md`、`plugin.host.md`、`docs/specs/upstram.sync.md`。
4. 游最重读的两个代码文件：`src-tauri/src/service/patch/chat_follow_anchor.rs`（补丁写法模板）和 `src-tauri/src/service/workflow/launch.rs`（补丁注册点）。

读完用 5 行内复述：我们做哪三件事、不碰哪三件事、补丁挂在哪条链路。

## 5. P-1 技术探针（1 天，只验证不写功能）

逐项做，每项给「方式 / 证据 / 结论 / 备选」：

| # | 探针 | 要点 |
|---|---|---|
| 1 | BatchBuffer 锚点 | 在官方 `dsh-client-ui-chat` 源码里定位流式 entry 的 push 点（旧分支 `feat-runtime-bootstrap` 的 diff 可当参照），输出 20 行上下文代码片段 |
| 2 | content-visibility 锚点 | 消息项容器的 CSS 类名在 bundle 里是否稳定（上游已打过 `:has()` 补丁说明它动过这里） |
| 3 | Tauri 2 per-webview 数据目录（多 Host 隔离用） | 最小 demo 验证；如果不可行，记录备选（清 cookie/storage 或 token 走 URL） |
| 4 | 旧分支移植清单 | 从 `aa3e283` 提取 ui-chat 的定制 diff，逐项标记"上游已有 / 需要新增 / 已废弃"，产出终稿 |

**探针全部有结论后停下来汇报，等我说"进 P2"。**

## 6. P2 性能补丁（汇报通过后再动）

- 照 `chat_follow_anchor.rs` 的风格，新写 `chat_stream_batch.rs` 与 `chat_content_visibility.rs`。
- 在 `workflow/launch.rs` 的既有补丁区按字母/功能位置注册。
- 用万级 DOM 会话实测：滚动 fps，切换 5 个会话均值。数字记进 `docs/perf/baseline-vs-mod.md`。

## 7. 硬规则

1. 补丁必须可定位可回滚：`ORIGINAL_*` / `PATCHED_*` 常量写全；在 `test/fixtures/` 放锚点样本（参照上游 `chat-follow-anchor-contract.mjs` 的方式）。
2. 中文文档注释 + 函数体内 `1.` `2.` `3.` 编号步骤注释；标识符保持英文。
3. 不提交凭据。上游 `packages/dsh-tauri-remote` 里有历史遗留的 `id_ed25519` 测试 key，**那是上游的**，我们这边不引入任何真 key。
4. 不自动 push、不打 tag、不发 release——除非我明确要求。
5. 改完必跑：`pnpm test`、`pnpm typecheck`、`pnpm build`、`cargo clippy -D warnings`、`cargo test`。
6. 不确定就停：影响范围大、不可逆、要拍板的，写清问题和选项等我。

## 8. 汇报格式（每阶段结束时）

```
阶段：P0 基线确认
环境：<真实输出>
基线：commit + main/inkdsh-mods 分支状态
观察：<启动 OK/NOK、首屏耗时、profile 落点、报错>
风险/未决：<列表>
下一步建议：<一句话>
```

现在开始第 1 节环境自检。
