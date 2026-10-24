# 本机性能基线（P2 唯一验收基准）

- 生成时间：2026-10-24
- 原始数据：`docs/perf/baseline-local.json`（可随时用同一命令重跑）
- 命令：

```bash
node test/perf/sweep.mjs --out docs/perf/baseline-local.json --runs 3 --seconds 8
```

## 1. 为什么这样测（方法与环境）

**结论先行：应用内基线暂时测不了，卡在「没有可路由模型」这道墙上；本基线测的是渲染层，
用与 WebView2 完全同版本的浏览器引擎，可复现、可回归。**

### 1.1 应用内为什么测不了

`dsh web` 打开一个磁盘上的会话需要 `agentAvailable: true`，而它要求存在可路由的
provider（`session/list` 返回 `agentAvailable:false`、`session/modelCatalog` 返回
`routableProviders: []`）。没有凭据时聊天区不渲染 transcript（点击会话只发
`commands/list`，不取会话内容；轨迹页显示「无计时数据」）。

> 顺带记一笔：我们合成的会话日志**能被核心正常识别**（`formatStatus: "current"`、
> `blank: false`，无解析错误），只是缺 provider 时前端不读它。等凭据到位即可直接复用。

### 1.2 渲染层替代方案

| 项 | 做法 | 为什么可信 |
|---|---|---|
| 引擎 | Playwright `channel: 'msedge'` | 本机 Edge 与 WebView2 都是 **155.0.4283.62**，同一套 Blink |
| CSS | 从官方 `dsh-client-ui-chat/lib/client.js` 里抽出运行时注入的 `css$16` 正文 | 与线上逐字节一致，不手抄 |
| 类名 | 解析 bundle 里的 `ChatView_module_css_default` 映射 | 不硬编码 `EvIC1a_` 哈希，核心升级后仍能跑 |
| DOM 形状 | 照 render 代码复刻：`[data-conversation-scroll] > .scroll > .column[data-chat-flow] > .flowItem[data-chat-flow-key] > [data-slot="conversation.chat.node"]` | 与官方渲染路径同构 |
| 内容 | 标题/段落/列表/表格/代码块/引用混排（每条 ~682px 高） | 覆盖真实回复的主要块类型 |

机器：Windows，显示 **165 Hz**（所以 fps 上限就是 165，不是 60）；
视口 1440×900 @ dpr1；headless Edge 155。

### 1.3 指标定义

| 指标 | 含义 | 为什么用它 |
|---|---|---|
| `fps` | rAF 回调频率 | 直观，但被刷新率封顶（本机 165） |
| `busyRatio` | rAF 回调内同步耗时 / 总时长 | **与 vsync 无关**，跨机可比 |
| `frameP95` | 帧间隔 95 分位 | 掉帧体验的主要来源 |
| `longTasks` / `longTaskMs` | >50ms 长任务 | 卡顿的硬指标 |
| `scrollHeight` | 滚动容器高度 | 校验 `contain-intrinsic-size` 估算误差 |

两种模式：
- **scroll**：以恒定步长来回滚动整列，每帧写 `scrollTop` 并读 `scrollHeight` 强制同步布局。
- **stream**：按目标频率（30Hz / 10Hz）整体重写**最后一条**消息的 DOM 子树，模拟一次
  token 提交（覆盖「重建 markdown 子树 + 布局」这段成本）。

## 2. 测量结果（3 次取中位数）

| items | DOM 节点 | content-visibility | 模式 | 频率 | fps | busyRatio | frameP95 | longTask | scrollHeight |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 300 | 10,510 | off | scroll | - | 165 | 0.002 | 6.2 | 0 | 206,269 |
| 300 | 10,510 | on | scroll | - | 164.9 | 0.002 | 6.2 | 0 | 69,673 |
| 300 | 10,510 | off | stream | 30Hz | 165 | 0 | 6.2 | 0 | 206,269 |
| 300 | 10,510 | on | stream | 30Hz | 165 | 0 | 6.2 | 0 | 43,878 |
| 1000 | 35,010 | off | scroll | - | 165 | 0.002 | 6.2 | 0 | 688,044 |
| 1000 | 35,010 | on | scroll | - | 165 | 0.003 | 6.2 | 0 | 202,629 |
| 1000 | 35,010 | off | stream | 30Hz | 160.7 | 0 | 6.2 | 0 | 688,044 |
| 1000 | 35,010 | on | stream | 30Hz | 165 | 0 | 6.2 | 0 | 115,278 |
| **2000** | **70,010** | **off** | scroll | - | 165 | **0.031** | 6.2 | 0 | 1,376,294 |
| **2000** | **70,010** | **on** | scroll | - | 165 | **0.002** | 6.2 | 0 | 390,808 |
| **2000** | **70,010** | **off** | stream | 30Hz | **103.5** | 0 | **24.2** | 0 | 1,376,294 |
| **2000** | **70,010** | **off** | stream | 10Hz | **143.4** | 0 | **18.2** | 0 | 1,376,294 |
| **2000** | **70,010** | **on** | stream | 30Hz | **165** | 0 | **6.2** | 0 | 217,278 |
| **2000** | **70,010** | **on** | stream | 10Hz | 165 | 0 | 6.2 | 0 | 217,278 |

完整数据（含每次 run 的原始值）：`docs/perf/baseline-local.json`。

## 3. 结论

1. **瓶颈门槛在 50k DOM 以上。** 10.5k / 35k DOM 下两种补丁都没有可见收益
   （165fps，busyRatio ≤0.3%）；到 70k DOM 才出现明确差距。
2. **补丁 B（content-visibility）单独就能把 70k DOM 流式场景拉满**：
   30Hz 流式 103.5 → 165 fps，`frameP95` 24.2 → 6.2 ms；滚动期主线程占用 3.1% → 0.2%。
3. **补丁 A（批量渲染）单独收益可测**：30Hz → 10Hz 即 103.5 → 143.4 fps，
   `frameP95` 24.2 → 18.2 ms。两者叠加后都在 165fps 封顶，说明**本机上两者功能重叠**，
   但补丁 A 的收益不依赖 `content-visibility` 是否被引擎支持（WebView2 个别版本对其有 bug），
   留作兜底是对的。
4. **`contain-intrinsic-size: auto 96px` 的估算误差很大**：70k DOM 时估算总高
   390,808 px vs 实际 1,376,294 px（**28%**）。`auto` 只对「渲染过」的条目生效，
   从未渲染的屏外条目统一按 96px 算 → 拖动滚动条/跳转 Turn 时位置会偏。
   本机 fixture 单条中位高 682px。等有真实会话数据后再定常数（见 §5）。
5. **旧方案 49–51 fps 不能作为本机基准**：本机 165Hz 屏 + 强 CPU，同等工作量下
   基线就是满帧。P2 的验收必须用本文件的数字（阈值见 §4）。

## 4. P2 验收阈值（按本机基线定）

| 场景 | 基线（未打补丁） | P2 目标 |
|---|---|---|
| 70k DOM + 30Hz 流式 | 103.5 fps / p95 24.2ms | **≥160 fps / p95 ≤6.5ms** |
| 70k DOM 滚动 busyRatio | 0.031 | **≤0.005** |
| 10k DOM 流式（日常会话） | 165 fps（已满） | 不回退 |
| 会话切换（5 个均值） | 未测（见 §5） | ≤500ms |

## 5. 未测项与解锁条件

| 未测 | 原因 | 解锁方式 |
|---|---|---|
| 应用内真实滚动/流式数字 | 无 provider → `agentAvailable:false`，transcript 不渲染 | 二选一：(a) 配一个真实 API Key（会消耗你的额度）；(b) 我起一个本地假 OpenAI 兼容服务并注册成 provider（零成本、可无限复现，推荐） |
| 5 个会话切换均值 | 同上（打不开会话） | 同上；合成会话 + `make-session.mjs` 已就绪 |
| React 重渲染次数 | 需在真组件里打点 | 解锁后可用 React DevTools 协议或 Perf 打点 |
| WebView2 与 Edge 的差异 | 未在 WebView2 里跑同一 fixture | 解锁后可让壳加载本地 fixture 页面再测一次 |

## 6. 可复现命令

```bash
# 生成 fixture（真实 CSS + 真实 DOM 形状；--cv 打开 content-visibility）
node test/perf/make-fixture.mjs --items 2000
node test/perf/make-fixture.mjs --items 2000 --cv

# 单次测量
node test/perf/scroll-fps.mjs --file test/perf/out/fixture-2000.html --mode scroll --runs 3
node test/perf/scroll-fps.mjs --file test/perf/out/fixture-2000.html --mode stream --hz 30 --runs 3

# 全量扫描（P2 改完后用同一条命令对比）
node test/perf/sweep.mjs --out docs/perf/baseline-local.json --runs 3 --seconds 8
```
