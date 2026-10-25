# 交接文档（2026-10-25 P3b 工作区）

> 给下一个会话 / 另一台机器用。仓库：`InkReturn/ink-dsh-desktop`，工作分支 **`feat/inkdsh-mods`**。
> 基线最后提交仍是 `e2b7d8b5`；**P3b 改动在工作区，未提交、未推送；debug 版已在真机完成登录、切换与原生缓存命中验收，release 未安装**。
> 环境与工具链要求见 `docs/baseline-p0.md §1`；补丁清单见 `docs/PATCH_REGISTRY.md`。

## 1. 现在处于哪一步

**P2 保持完成（release 冷启动测量仍未做）；P3b 多 Host + 远程缓存、P3c 后台预加载与闲时暂停已实现并通过真实 Host 验收。P3d 断网渲染未开始。**

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
| **release 冷启动（≤10s 门槛）** | ⏳ **未完成**（用户要求本次跳过） | 待补 `docs/perf/baseline-local.md` |
| P3a 上游侦察 | ✅ | `plugins-sync` 是推插件，不是缓存；gateway 保留鉴权/Cookie/WebSocket |
| **P3b + 直连多 Host（工作区）** | ✅ 真实登录 / 保活切换 / 原生缓存命中已验证 | `packages/dsh-tauri-remote/src/host/service/{direct,static-cache}.ts`；`src-tauri/src/desktop/remote_cache.rs`；详见 `PATCH_REGISTRY.md`、`docs/perf/remote-cache.md` |
| **P3c 后台预加载（工作区）** | ✅ 插件 bundle 缓存 + 闲时暂停；二次访问 LAN 113ms / Inkmo 716ms | `packages/dsh-tauri-remote/src/host/service/prefetch.ts`；`gateway.ts` 活动统计；`static-cache.ts`/`remote_cache.rs` 插件 key |

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

## 3. P3b 真机验收结果（2026-10-25 已完成）

1. 两个直连 Host 均已用 `dsh-web-login` 密码登录真实连通：
   - `http://192.168.31.83:3080/`（LAN HTTP）
   - `https://dshs.inkmo.top/`（公网 HTTPS）
   凭据只存在 Host 配置中，未写入 iframe URL、缓存、meta 或本文件。
2. Host 切换器只显示手工添加项；LAN / Inkmo 均可切换，登录状态保持，无需重复输入密码。
3. 本机、LAN、Inkmo 三态都实际截图验收：iframe 满高、无本机/远端叠加、无旧页面残留。
4. 保活切换实测 **51–88ms**；切换后 150ms 截图页面完整。关键修复：远端帧从 `display:none` 改为 `visibility:hidden`，避免 WebView2 暂停非活动 iframe 合成导致切回首帧只有背景。
5. 原生缓存命中实测（`RUST_LOG=debug` 的 `inkdsh::remote_cache` 日志）：LAN 与 Inkmo 首次加载各 **4 个 `/assets/*` hit=true**，HTML `/` hit=false 按设计回源。修复点：绑定远端 Host 时必须清空本机 preload 的 `frontend` 状态，否则远程缓存分支永远不可达。
6. 首连耗时：LAN **5.4s** 到插件 boot ready；Inkmo **20.2s**，其中远端插件 boot 本身约 20s，壳层加载遮罩不再额外等待（`plugin-boot:ready` + 20s 兜底）。
7. 实际链路是 `壳 iframe → 本机 gateway → 远端`，没有用浏览器直开远端地址冒充验收。
8. 缓存根：`config::get_base_dir(app)/inkdsh/remote-cache/<hostId>/<sha256(origin)前16位>/<sha256(path)>`。性能表与证据见 `docs/perf/remote-cache.md`。
9. P3c 后台预加载/闲时暂停已完成：连接后 5s 开始，预热 `/assets/*` 与 `/plugins/??` 合并 bundle；缓存完整即跳过；用户请求活跃时暂停。P3d 断网渲染**未做**，当前 HTML 不缓存，不声称离线可用。
10. P3c 只在连接成功后触发；应用启动时不会自动重连未连接的 Host。二次访问实测：LAN 点击到目标 iframe 113ms；Inkmo iframe 到 ready 497ms、点击到 ready 716ms。

```bash
export PATH="/c/Users/inkre/AppData/Roaming/fnm/node-versions/v24.16.0/installation:$PATH"
pnpm exec vitest run --project unit \
  packages/dsh-tauri-remote/src/host/service/direct.test.ts \
  packages/dsh-tauri-remote/src/host/service/direct.integration.test.ts \
  packages/dsh-tauri-remote/src/host/service/static-cache.test.ts \
  packages/dsh-tauri-remote/src/host/service/gateway.test.ts \
  packages/dsh-tauri-remote/src/host/service/gateway.utils.test.ts \
  packages/dsh-tauri-remote/src/host/service/machine.test.ts \
  packages/dsh-tauri-remote/src/client/components/machines-section.test.tsx \
  packages/dsh-tauri-remote/src/client/apis/parsers.test.ts src/hooks/use-remote.test.ts
# 2026-10-26 最终实测：11 files / 481 tests 全部通过
```

## 4. 其它未完成 / 待办

1. **release 冷启动测量**：`CARGO_INCREMENTAL=0 pnpm exec tauri build --no-bundle`（约 10–20 分钟，会重编 release 依赖），产物在 `src-tauri/target/release/`。启动后从日志读 `STARTUP_READY … spawn_to_ready_ms`，门槛 **≤10s**（release 构建日志在 `%APPDATA%\dsh-tauri\logs\`，不是 `dev\`）。数字写进 `docs/perf/baseline-local.md §7.6`。
2. **P4 收尾**：把 `docs/PATCH_REGISTRY.md` 的内容合并进 `docs/inkdsh-plan` 分支的 `FORK_BASELINE.md`（那是方案文档分支，本分支没有该文件）。
3. **P3d（未开始）**：HTML 缓存、断网切到缓存 Host 的离线提示与不发请求；P3c 已完成代码、自动化与 debug 真机验收。

## 5. 本次测试实况（2026-10-25）

- `pnpm typecheck` ✅；`pnpm build` ✅（前端 12.92s；`dsh-tauri-remote` 构建通过）。
- P3 相关 Vitest **481/481**（11 files，含 P3c 预加载 4 条）；切换/保活/布局 **46/46**；Rust `desktop::` **73/73**。
- 变更 TypeScript 文件 eslint 无输出；变更 Rust 文件 clippy 无告警（`service/workflow/process.rs` 的上游 unused import 不计入本功能）。
- 历史全量（早于最后 3 处修复）Rust **1225 passed / 1 failed / 1 ignored**：仅 `config::proxy::tests::test_treats_an_empty_proxy_as_a_direct_connection`，外部探测返回 502，单跑同样失败，与本功能无关。
- 全量前端 **5408 passed / 18 failed / 5 skipped**：本会话引入的 3 条 mock 污染已修复；其余 15 条为 Windows symlink 权限/既有测试（`bootstrap`、`migrate`、`plugins-sync`、`scroll-follow-patch`），不可写成全量全绿。

## 6. 中断现场（重要）

- **release 构建被手动中止**（`cargo` 进程已杀）。`src-tauri/target/release/` 可能有半成品，下次直接重跑上面的命令即可（增量）。
- 后台可能还有这些**可安全杀掉**的残留进程：
  - `dsh web --port 3097`（dev profile 的独立实例，我起的）
  - `dsh web --port 3098`（隔离性能环境 `D:/inkdsh-perf/home`，我起的；token 存在 `/tmp/perftoken`，重启后会变）
  - `node test/perf/fake-model.mjs --port 4010`（假模型，零成本）
  - `tauri dev`（开发壳；用户可能正在用）
- **磁盘**：D: 曾经写满导致两次构建失败（`os error 112`）。用户已清理，现在 58%。构建缓存可用 `CARGO_INCREMENTAL=0` 规避 637MB 的 incremental 目录。

## 7. 复现性能测量的完整配方

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

## 8. 下次开工建议的 skills

- `agentic-coding`（微步提交 + 验收合同）
- `systematic-debugging` / `debugging-observability`（如果补丁在 WebView2 上出怪相）
- `report-three-blocks`（阶段汇报）
