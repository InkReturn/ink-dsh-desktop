# inkdsh 魔改方案（基于 fork dsh-tauri）

- 版本：v1.0
- 日期：2026-10-24
- 状态：可交接实施
- 配套文件：`FORK_BASELINE.md`（上游基线记录，先读）、`BOOTSTRAP_FORK_PROMPT.md`（开工提示词）

---

## 1. 这份方案是什么

`InkReturn/ink-dsh-desktop`（fork 自 `dsh-tauri/deepseek-harness-desktop`，基线 v0.25.0 @ `47f03f4`）上做魔改。

**形态：加法，不是减法，不是重写。**

- 上游功能全部保留：桌面壳、sidecar 生命周期、资源预载、补丁体系、多主机远程、桌宠、移动端、通知、调度、worktree……一个都不删。
- **我们要加的只有三样**，全部落到上游已有的扩展点上：
  1. 流式消息 100ms 批量渲染
  2. 屏幕外消息 `content-visibility` 渲染跳过
  3. 会话投影预折叠（session-preload，秒开）
- 皮肤兼容暂不做（决策 3），后面重搓皮肤。
- 已完成的旧工作 `feat-runtime-bootstrap`（aa3e283）作为**移植来源**，有价值的东西 cherry-pick 过来，不做 base。

---

## 2. 魔改清单（按上游风格落位）

### 2.1 性能补丁 × 2（核心目标：长会话不卡）

上游已有 `chat_follow_anchor.rs`（去 `:has()` + 滚动镜像）和 `scroll_follow.rs`，但缺流式批量更新与视窗外跳过。我们补这两块。

#### 补丁 A：流式批量渲染 `chat_stream_batch.rs`

| 项 | 内容 |
|---|---|
| 目标文件 | 核心安装目录 `node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js`（同上游其他 patch 的目标约定） |
| 锚点 | 上游官方源码中 chat 的流式 entry 逐一 push 处（移植前先在官方源码里定位，旧分支 diff 可作参照） |
| 行为 | 用 BatchBuffer：流式 token 先入缓冲，每 100ms 批量提交一次进 React 状态 |
| 参数 | 批次间隔通过 `service::config` 读一个开关（不写死），默认 100ms |
| 失败行为 | 补丁未命中锚点 → `PatchOutcome::Skipped`，warn 日志，不阻断启动 |

#### 补丁 B：content-visibility 渲染跳过 `chat_content_visibility.rs`

| 项 | 内容 |
|---|---|
| 目标 | 官方 chat 的消息项容器（`.flowItem` 一节），注入 `content-visibility: auto; contain-intrinsic-size: <估算>` |
| 锚点 | 官方 CSS Modules 编译出的类名（**锚点脆**——P-1 必须先核实类名稳定性，不稳就换数据属性锚点） |
| 预验证 | 在万级 DOM 会话上测滚动 fps：补丁前 49–51 → 目标 ≥55；不达标就调整或回退 |
| 失败行为 | 同上，失败仅告警 |

**验证标准**：用 `dsh-desktop-research/perf-*.js` 的同款脚本，在相同硬件（或等效）上复测；数据写进 `docs/perf/baseline-vs-mod.md`。

### 2.2 会话投影预折叠（核心目标：切会话秒开）

| 项 | 内容 |
|---|---|
| 现状 | 上游没有此能力；旧分支 `plugins/session-preload` 已实现思路（读 `session_projcache`、预折叠、内存缓存） |
| 落位 | **两个候选，P-1 探针后定**：(a) 新 patch 模块 `session_preload_patch.rs` 改核心 session list 读路径；(b) 在 `service::workflow::launch` 的就绪流程里通过既有 hook 触发预折叠 |
| 边界 | **只读**会话投影缓存，不修改格式，不写缓存文件（与旧方案边界一致） |
| 指标 | 5 个会话切换均值 ≤500ms（命中预载时） |

### 2.3 不做的（本阶段）

- 皮肤兼容（决策 3）
- 重写 UI（任何"官方 chat 不够好，自己画一套"的念头都是越界）
- 删上游包（决策 4）
- 改官方内核/会话存储格式

---

## 3. 工程流程

### 3.1 分支模型

```
main（冻结在 47f03f4）          ← 只跑上游 sync
  │
  └── feat/inkdsh-mods           ← 魔改主分支
        ├── feat/p0-baseline     ← 基线确认
        ├── feat/p1-probes       ← 技术验证
        ├── feat/p2-perf         ← 两个性能补丁
        ├── feat/p3-preload      ← 会话预折叠
        └── feat/p4-acceptance   ← 总验收
```

### 3.2 补丁编写规范（照上游既有风格）

参照 `service/patch/chat_follow_anchor.rs`：

1. `const ORIGINAL_*: &str = r#"..."#` 与 `PATCHED_*: &str = r#"..."#`：**锚点原文与替换文本必须可从官方源码逐字复制**。
2. 每个补丁一个文件，`pub(crate) fn apply(app: &AppHandle) -> Result<(), String>`，内部调 `crate::utils::patch_dsh`。
3. 在 `workflow/launch.rs` 的补丁区（`::apply(&app_handle)` 那段，`if let Err(e) = ... log::warn!` 一行）注册，保持按字母序或功能分组。
4. 补丁前先在 `test/fixtures/` 放一份锚点样本；补丁函数要能在 fixture 上验证（上游 `chat-follow-anchor-contract.mjs` 就是这种模式）。
5. 失败/未命中只告警，不 panic；官方升级后锚点失配 → 跳过并提示，而不是搞坏启动。

### 3.3 每次改动必跑

```powershell
pnpm test               # 仓库根，注意它包含哪些套件
pnpm typecheck
pnpm build
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

---

## 4. 分阶段计划

### P0：基线确认（0.5 天）

- 目标：克隆 fork → `pnpm install` → `pnpm dev` → 应用能启起来看到 DSH 界面。
- 记录：启动耗时、`docs/spikes.md` 记下 `frontend_preload` 是否生效、`desktop profile` 落在哪个 profile 目录。
- 产出：基线报告（机器、Node、pnpm、启动 OK/NOK、首屏耗时数字）。
- 不做任何代码修改。

### P-1：技术探针（1 天）

把旧方案的 6 项缩到 4 项（①④⑤上游已做过，直接确认即可）：

| # | 探针 | 产出 |
|---|---|---|
| 1 | 官方 `dsh-client-ui-chat` 流式渲染锚点定位（BatchBuffer 的注入点），导出代码片段 | `docs/spikes.md`，附 20 行上下文代码 |
| 2 | 消息项 CSS 类名是否稳定（`:has()` 补丁的锚点旁），`content-visibility` 锚点选型 | 锚点常量片段 + 落选原因 |
| 3 | Tauri 2 per-webview 数据目录（多 Host 隔离）是否可用；`frontend_preload` 在多窗口下的行为 | 结论 + 选型，影响以后多 Host 深化（本阶段只做单 Host 优化，但探针先跑） |
| 4 | 旧分支 diff 全文提取（aa3e283 里 ui-chat 相对官方的定制点清单），逐个标记"上游已有/需要新增/已废弃" | 移植清单终稿 |

### P2：性能补丁（3–4 天）

- 写 `chat_stream_batch.rs`、`chat_content_visibility.rs`，按 §3.2 风格注册。
- 目标：旧机实测基线 49–51 fps → 本机复测滚动 ≥55 fps；切换 5 个会话均值对比。
- 性能前后对照写进 `docs/perf/baseline-vs-mod.md`。

### P3：会话投影预折叠（3–4 天）

- 按 P-1 探针 4 的选型落地。
- 指标：切会话 ≤500ms（命中时），不破坏上游 session 流程。

### P4：总验收（1–2 天）

- 过一遍 §6 验收表。
- 更新 `FORK_BASELINE.md` 的补丁清单表。
- 决定是否向上游提 PR（性能补丁可能通用）。

---

## 5. 风险

| 风险 | 对策 |
|---|---|
| 上游补丁锚点随官方 rc.x 升级失效 | 补丁全走 `patch_dsh` 的 best-effort 通道；维护 `FORK_BASELINE.md` 锚点清单 |
| BatchBuffer 与上游流式协议不兼容（如 turn 事件结构变了） | P-1 探针先验证；不兼容就放弃该补丁，不做架空的"兼容性封装" |
| `content-visibility` 在 WebView2 155 上有 bug（已知某些版本对动态插入节点表现差） | P-1 先跑 fps 对照；不达标就撤这个补丁，保留批量渲染也行 |
| 预折叠读取了被截断的 projcache（之前事故的遗留） | 解析失败静默跳过，一律不删不改 |
| 把 fork main 当工作分支用了 | 建分支前检查 `git branch --show-current`；分支模型图贴在 `FORK_BASELINE.md` |

---

## 6. 验收总表（P4 过一遍）

- [ ] `pnpm dev` 冷启动到界面 ≤ 5s（含核心自愈流程；机器基线记录）
- [ ] 万级 DOM 会话滚动 ≥55 fps
- [ ] 5 个会话切换均值 ≤500ms（命中预载）
- [ ] 补丁全部按 best-effort 走，任意一个补丁失配都能启动成功
- [ ] `pnpm test` / `typecheck` / `build` / `cargo clippy` / `cargo test` 全绿，记录实际用例数
- [ ] `FORK_BASELINE.md` 新增补丁登记完整
- [ ] 没有任何一处写用户 `~/.dsh` 下的既有文件（路径断言或人工核对）
