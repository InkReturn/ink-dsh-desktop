# 交接文档（2026-10-25 04:xx 本机状态）

> 给下一个会话 / 另一台机器用。仓库：`InkReturn/ink-dsh-desktop`，工作分支 **`feat/inkdsh-mods`**（已推送到远端）。
> 环境与工具链要求见 `docs/baseline-p0.md §1`；补丁清单见 `docs/PATCH_REGISTRY.md`。

## 1. 现在处于哪一步

**P2 基本完成，还差一条测量**（release 冷启动口径）。P3 未开始。

| 阶段 | 状态 | 证据位置 |
|---|---|---|
| P0 基线 | ✅ | `docs/baseline-p0.md` |
| P-1 四项探针 | ✅ | `docs/spikes.md` |
| 本机性能基线（渲染层） | ✅ | `docs/perf/baseline-local.md` §1–§2、`docs/perf/baseline-local.json` |
| P2 补丁 A（流式批量，节点层） | ✅ 已提交 `4fe63c04` | `src-tauri/src/service/patch/chat_stream_batch.rs` |
| P2 补丁 B（content-visibility，哈希无关锚点） | ✅ 已提交 `be036539` | `src-tauri/src/service/patch/chat_content_visibility.rs` |
| A+B 合并验证 | ✅ 真机已生效 | 页面里有 `<style id="inkdsh-content-visibility">`；`STARTUP_READY 94/94` |
| 本机插件安装（21 个） | ✅ 20 个激活（`dsh-plugin-ide` 宿主半失败，与本 fork 无关） | `docs/PLUGIN_INSTALL_LOCAL.md` |
| clippy 口径 | ✅ 改为「我们改的文件零告警」，21 条上游债登记 | `docs/lint-debt.md` |
| **release 冷启动（≤10s 门槛）** | ⏳ **未完成**（构建被中断，见 §4） | 待补 `docs/perf/baseline-local.md` |

## 2. 已验证的命令与数字（可直接复用）

```bash
export PATH="/c/Users/inkre/AppData/Roaming/fnm/node-versions/v24.16.0/installation:$PATH"   # Node 24.16 + pnpm 11.7
cd src-tauri
CARGO_INCREMENTAL=0 cargo test --lib      # 1222 passed / 0 failed / 1 ignored
cargo clippy --lib | grep -E "chat_stream_batch|chat_content_visibility|patch/mod.rs|workflow/launch.rs"   # 期望：无输出
```

| 指标 | 值 |
|---|---|
| 补丁 B：70k DOM 滚动 busyRatio | 0.031 → **0.002** |
| 补丁 B：70k DOM 流式 30Hz | 103.5 → **165 fps**（p95 24.2 → 6.2ms） |
| 补丁 A：契约（500ms/50 token） | 提交 **≤6 次**（未打补丁 50 次） |
| 应用内 5 会话冷开切换 | 均值 **234ms** / 最大 299ms |
| 应用内滚动 / 流式 | 159–164 fps（3.5k DOM） |
| `pnpm typecheck` | 通过 |
| `pnpm build` | 通过（12.9s） |
| `pnpm test:unit` | 5385 passed；**16 failed 全部是上游既有**（基线点同样 15 条 + 1 条 `gateway.test.ts` 端口竞态，单跑通过） |

## 3. 未完成 / 待办（按优先级）

1. **release 冷启动测量**：`CARGO_INCREMENTAL=0 pnpm exec tauri build --no-bundle`（约 10–20 分钟，会重编 release 依赖），产物在 `src-tauri/target/release/`。启动后从日志读 `STARTUP_READY … spawn_to_ready_ms`，门槛 **≤10s**（release 构建日志在 `%APPDATA%\dsh-tauri\logs\`，不是 `dev\`）。数字写进 `docs/perf/baseline-local.md §7.6`。
2. **P4 收尾**：把 `docs/PATCH_REGISTRY.md` 的内容合并进 `docs/inkdsh-plan` 分支的 `FORK_BASELINE.md`（那是方案文档分支，本分支没有该文件）。
3. **P3（未开始）**：多 Host 切换 + 远程资源缓存，任务书在 `docs/inkdsh-plan:docs/inkdsh/MOD_PLAN.md` P3a–P3d。先读 `packages/dsh-tauri-remote` 的 `plugins-sync.ts` / `tunnel.ts`，再动 `frontend_preload.rs`。

## 4. 中断现场（重要）

- **release 构建被手动中止**（`cargo` 进程已杀）。`src-tauri/target/release/` 可能有半成品，下次直接重跑上面的命令即可（增量）。
- 后台可能还有这些**可安全杀掉**的残留进程：
  - `dsh web --port 3097`（dev profile 的独立实例，我起的）
  - `dsh web --port 3098`（隔离性能环境 `D:/inkdsh-perf/home`，我起的；token 存在 `/tmp/perftoken`，重启后会变）
  - `node test/perf/fake-model.mjs --port 4010`（假模型，零成本）
  - `tauri dev`（开发壳；用户可能正在用）
- **磁盘**：D: 曾经写满导致两次构建失败（`os error 112`）。用户已清理，现在 58%。构建缓存可用 `CARGO_INCREMENTAL=0` 规避 637MB 的 incremental 目录。

## 5. 复现性能测量的完整配方

```bash
# 1. 假模型（OpenAI SSE，可控 token 速率）
node test/perf/fake-model.mjs --port 4010 --chunks 400 --interval 8

# 2. 隔离数据根 + 本地 provider（不碰 ~/.dsh.dev）
#    D:/inkdsh-perf/home/profiles/{web,headless}/cordis.patch.yml 已配好 perf-local 路由
#    启动时必须带上 PERF_LOCAL_API_KEY=dummy（pi-ai 的 openai-completions 要求有 key）
cd D:/inkdsh-perf/workspace
PERF_LOCAL_API_KEY=dummy DSH_HOME='D:/inkdsh-perf/home' dsh web --no-open --port 3098

# 3. 造真会话（内核自己写日志，才会进索引、能被 UI 打开）
node test/perf/seed-sessions.mjs --count 2 --turns 60

# 4. 测量
node test/perf/app-measure.mjs --url "http://127.0.0.1:3098/?token=…" --mode switch --cold
node test/perf/app-measure.mjs --url "…" --mode scroll --seconds 8 --runs 2
node test/perf/app-measure.mjs --url "…" --mode stream --seconds 10

# 5. 渲染层 fixture（补丁 A/B 的真实基准）
node test/perf/sweep.mjs --out docs/perf/baseline-local.json --runs 3 --seconds 8
node test/perf/cv-effects.mjs --file test/perf/out/fixture-2000-cv.html --mode stream-insert
node test/perf/cv-effects.mjs --file test/perf/out/fixture-2000.html --mode scrollbar-jump
```

## 6. 下次开工建议的 skills

- `agentic-coding`（微步提交 + 验收合同）
- `systematic-debugging` / `debugging-observability`（如果补丁在 WebView2 上出怪相）
- `report-three-blocks`（阶段汇报）
