# P-1 技术探针（inkdsh 魔改）

- 日期：2026-10-24
- 分支：`feat/inkdsh-mods`
- 探针对象：官方核心 `@deepseek-ai/dsh-client-ui-chat@0.2.1-rc.2` 的发布产物
  `node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js`
  （md5 `c0343309506321741ac1ca68cc39b86b`，与 `npm pack` 原始发布物逐字节一致，
  未被任何补丁污染；下文行号均指该文件）
- 结论摘要：**补丁 A 的注入点必须改到节点订阅层（不是旧实现的 entries 层）**；
  补丁 B 有一个哈希无关的稳定锚点；探针 3 结论「Windows/Linux 可行，macOS 缺」。

---

## 探针 1：BatchBuffer 锚点（流式批量渲染）

### 方式

1. 用 `npm pack @deepseek-ai/dsh-client-ui-chat@0.2.1-rc.2` 取原始发布物，md5 比对确认
   机器上那份 `lib/client.js` 是干净的官方产物。
2. 在产物里定位渲染链路：`ChatView` → `renderSlot("conversation.chat.flow", { entries, … })`
   → `ChatFlow` → 逐条 `entry` → `ChatNodeSeat`。
3. 反向核对数据来源：`ChatSnapshotBuilder.apply()`（`client.js:9354`）→
   `MutableChatNodeStore.upsert()`（`client.js:8607`）→ `dirtyKeys` →
   `MutableChatNodeStore.publish()`（`client.js:8629`）→ 逐 key `source.publish()`，
   订阅方是 `ChatNodeSeat` 的 `useChatNode(nodeKey)`。
4. 对照旧分支 `origin/feat-runtime-bootstrap@aa3e283` 的实现
   （`plugins/ui-chat/src/official/client/chat/ChatFlow.tsx` + `plugins/ui-chat/src/virtual.ts`）。

### 证据

注入点（`client.js:3712`，缩进是 2 个 tab，`→` 仅用于这里展示）：

```
→→* Subscribe, apply Turn-process visibility, and dispatch one stable Context key.
→→* Policy reads select this seat's own conclusion, so a mode change re-renders
→→* only seats whose visibility actually changes.
→→*/
→→const ChatNodeSeat = (0, react.memo)(function ChatNodeSeat({ nodeKey, groupPart, useChatNode, useChatNodeProcess, usePresentation, cwd, openFile, inspectCall, forkAt, loadImage, fileMentions, useStore, actions, renderSlot, t, useChatNodeBottom, deferCollapse, useGroupAction }) {
→→→const bottom = useChatNodeBottom(nodeKey);
→→→const node = useChatNode(nodeKey);          // ← 每个流式 token 都从这里进入重渲染
→→→const routedNode = node;
→→→const turn = turnOf$1(routedNode);
→→→const processPresentation = useChatNodeProcess(nodeKey);
→→→const processSpec = processPresentation?.spec;
→→→const storedOpen = useStore((state) => processSpec !== void 0 && turnProcessOpen(state, processSpec, bottom === true && deferCollapse));
→→→const liveProcess = processPresentation !== void 0 && !processPresentation.turnClosed;
→→→const interleavedInput = processPresentation?.hasInterleavedInput === true;
→→→const alwaysOpen = liveProcess || interleavedInput || turnProcessAlwaysOpen(routedNode);
→→→const processOpen = alwaysOpen || storedOpen;
→→→const setOpen = (0, react.useCallback)((open) => {
→→→→if (processSpec !== void 0 && !alwaysOpen) actions.setTurnProcessOpen(processSpec.turn, processSpec.answerStep ?? 0, open);
```

旧分支的实现（等价于把「entries 数组」压批）：

```ts
// plugins/ui-chat/src/official/client/chat/ChatFlow.tsx（旧分支）
function useBatchedEntries(entries) {
  const [visible, setVisible] = useState(entries)
  const buffer = useRef(null)
  if (buffer.current === null) buffer.current = new BatchBuffer(next => setVisible(next), 100)
  useEffect(() => { buffer.current?.push(entries) }, [entries])
  ...
}
const visibleEntries = useBatchedEntries(entries)   // rows 与 tail 都改用它
```

**关键发现（与旧实现的偏差）**：`ChatSnapshotBuilder.apply()` 把每个 stream delta 分成
`structural`（新增/移动节点 → `order` 变）和 `contentOnly`（同 key、内容变 → `order` 不变）。
纯文本流式输出走的是 `contentOnly`，此时 `entries`/`order` **引用不变**，
`ChatFlow` 不重渲染；真正逐 token 重渲染的是 `ChatNodeSeat` 通过
`useChatNode(nodeKey)` 订阅到的节点源。**旧 BatchBuffer 压的是 entries，对 token 流式基本无效**，
只在「一次涌入大量新节点」（工具调用、并发 part）时有效。

### 可用的锚点（全部在官方产物里各出现 1 次，已用 `grep -c` 校验）

| # | 锚点原文（`\t` 为真实 tab） | 用途 |
|---|---|---|
| A1 | `\t\t\tconst node = useChatNode(nodeKey);` | 节点级压批（主锚点） |
| A2 | `\t\tconst ChatNodeSeat = (0, react.memo)(function ChatNodeSeat({ nodeKey, …` | 在组件外插入缓冲实现 |
| A3 | `\t\t\tconst rows = entries.map((entry) => {` | entries 级压批（可选，覆盖结构性涌入） |
| A4 | `\t\t\tconst tail = entries.at(-1);` | 与 A3 配对（尾部判断要用同一个快照） |

### 结论

1. 补丁 A 落 **A1**（+ A2 放实现）：把 `useChatNode(nodeKey)` 的返回值过一层
   `useBatchedValue`（100ms，与旧 `BatchBuffer` 同语义：定时器一旦启动不因新值重置，
   卸载时 `flushNow()`），可把 token 级重渲染压到 ≤10 次/秒。
2. A3/A4 作为可选第二组锚点，只在结构性涌入时有用；先上 A1，实测不够再加，
   避免一次改两处、回滚粒度变粗。
3. 语义安全性：`BatchBuffer` 保证「最后一个值必在 ≤100ms 内提交」，不会丢终态；
   只影响渲染快照，不影响 `order`/`navigation`/滚动跟随等业务状态。

### 备选方案

- **备选 1（更靠近数据源）**：改 `MutableChatNodeStore.publish()`（`client.js:8629`），
  在 `dirtyKeys` 非空时延迟 100ms 再 `source.publish()`。优点：一处改动覆盖所有订阅者；
  缺点：`publish()` 同时驱动 bottom/process/turnKind 与导航高亮，延迟面更大，回归风险高。
- **备选 2（放弃批处理）**：只做探针 2 的 `content-visibility`。若 P2 实测 token 级重渲染
  不是瓶颈，这条最省事。

---

## 探针 2：content-visibility 锚点

### 方式

1. 官方 chat 的 CSS 是运行时注入的 `<style data-plugin-css="@deepseek-ai/dsh-client-ui-chat/ChatView.module.css">`
   （`client.js:2676-2686`），类名来自 CSS Modules 哈希（当前前缀 `EvIC1a_`）。
2. 消息项容器 = `div.flowItem`，带稳定 data 属性
   `data-chat-flow-key` / `data-chat-node-key` / `data-chat-flow-kind`（`client.js:3804-3811`）。
3. 对照旧分支 `plugins/ui-chat/src/official/client/chat/ChatView.module.css` 的定制点。

### 证据

官方 `.flowItem` 规则（在 `css$16` 字符串里）：

```
.EvIC1a_flowItem{min-width:0}
```

旧分支的定制（就加在同一条规则上）：

```css
.flowItem {
  min-width: 0;
  /* 渲染层虚拟化：屏幕外的 flow 行跳过布局与绘制，但保留 DOM… */
  content-visibility: auto;
  contain-intrinsic-size: auto 96px;
}
```

类名稳定性判断：

| 锚点形式 | 稳定性 | 说明 |
|---|---|---|
| `.EvIC1a_flowItem` 字面量 | **不稳** | `EvIC1a` 是 CSS Modules 哈希，核心版本一变就换 |
| `ChatView_module_css_default.flowItem`（模块映射对象，`client.js:2688-2710`） | 稳 | 哈希无关，渲染代码就在用它 |
| `[data-chat-flow-key]` / `[data-chat-flow-kind]` | 稳 | 语义化 DOM 属性，供滚动/导航逻辑使用 |

### 结论

**推荐做法：在官方注入 CSS 之后追加一条独立 `<style>`，类名取自模块映射对象**，不碰原 CSS 文本：

- 锚点 B1（`client.js:2677`，唯一）：
  `\t\tconst tagId$16 = "@deepseek-ai/dsh-client-ui-chat/ChatView.module.css";`
  在其所在的注入代码块之后追加：

```js
		if (typeof document !== "undefined" && document.getElementById("inkdsh-content-visibility") === null) {
			const cv = document.createElement("style");
			cv.id = "inkdsh-content-visibility";
			cv.textContent = "." + ChatView_module_css_default.flowItem + "{content-visibility:auto;contain-intrinsic-size:auto 96px}";
			document.head.appendChild(cv);
		}
```

- 备选锚点 B2（哈希绑定，仅当 B1 失配时用）：
  把 `css$16` 里的 `.EvIC1a_flowItem{min-width:0}` 就地替换成
  `.EvIC1a_flowItem{min-width:0;content-visibility:auto;contain-intrinsic-size:auto 96px}`。

风险（写进 P2 验收）：`content-visibility:auto` 会让屏幕外行**跳过布局**，而官方的滚动跟随/
`nth-last-child` 间距规则依赖真实高度；`contain-intrinsic-size: auto 96px` 会记忆上次渲染尺寸，
但首屏新插入的高行仍可能跳动。P2 必须做「滚动跟随 + 跳转 Turn 导航」回归。

---

## 探针 3：Tauri 2 per-webview 数据目录（多 Host 隔离）

### 方式

1. 查本仓库锁定的 tauri / wry 版本：`tauri 2.12.3`、`wry 0.57.0`、`tao 0.37.1`。
2. 在 cargo registry 里核对 API 与平台实现。
3. 查本 fork 现状：壳层窗口怎么设数据目录、远程 Host 用什么承载。

### 证据

- `tauri-2.12.3/src/webview/webview_window.rs:1089` 存在 `pub fn data_directory(PathBuf)`；
  `webview/mod.rs:1028-1044` 同样有同名方法（WebviewBuilder 侧）。
- wry 平台实现：
  - Windows：`src/webview2/mod.rs:293-351` 把 `data_directory` 传给 WebView2 environment；
  - Linux：`src/webkitgtk/web_context.rs:32-43` 用 `base_cache_directory` / `base_data_directory`；
  - macOS：**无**该实现，只有 `data_store_identifier`（macOS 14+）可替代
    （wry `src/lib.rs:1615` 注释明说）。
- 约束（wry `src/web_context.rs:42`）：**不同 `CoreWebView2EnvironmentOptions`（例如
  `additional_browser_args` 不同）的实例必须使用不同 data_directory**；反过来「不同目录」
  永远合法。
- 本 fork 现状（`src-tauri/src/desktop/builder.rs:81-101`）：已经用
  `.data_directory(webview_data_directory(app))` + `.additional_browser_args(WINDOWS_SHELL_BROWSER_ARGS)`
  （`builder.rs:888-889`），且注释明确「壳层窗口必须共用」同一目录（避免 WebView2 管道
  `0x8007139F`）。
- 多 Host 当前形态：远程 Host 渲染在**同一个 webview 的 iframe** 里
  （`src/layout/components/iframe.tsx:233-256`，`remoteMode` → `frameSrc = srcOverride`，
  `http://<host>:3081`）。不同 origin 天然隔离 localStorage/cookie/IndexedDB。

### 结论

1. **per-webview 数据目录在 Windows（WebView2）与 Linux（WebKitGTK）可用**，本 fork 已经在用，
   API 与版本都对得上；无需自研。
2. **macOS 不可用**，只能 `data_store_identifier`（macOS 14+）或退化为「清 storage / token 走 URL」。
3. 就本阶段目标（单 Host 性能）而言**不需要**动它：多 Host 走 iframe，隔离靠 origin 已完成。
   只有将来把远程 Host 升级成独立 webview 窗口时，才需要「一 Host 一 data_directory」，
   并且必须与 `additional_browser_args` 的组合保持一致（同参数才能共用目录）。
4. 待验证（留到真正做多 Host 深化时）：静态资源预载 `frontend_preload`（Windows
   `WebResourceRequested` 过滤）在多窗口下是否会跨窗口误拦；本次未展开。

---

## 探针 4：旧分支移植清单（`aa3e283` → 现状）

### 方式

1. 取 `origin/feat-runtime-bootstrap:plugins/ui-chat/lib/client.js`（旧分支的构建产物）与
   官方 `0.2.1-rc.2` 产物对比，逐项确认定制点。
2. 旧分支 `src/official/**` 是从官方包 sourcemap 还原的 TS 源，用作语义参考（不作为移植源）。

### 清单终稿

| # | 旧分支定制点 | 现状（官方 0.2.1-rc.2 + 上游补丁） | 处置 |
|---|---|---|---|
| 1 | `BatchBuffer` + `useBatchedEntries`（`ChatFlow`，100ms，压 entries） | 官方无；但如上分析，热点在 `useChatNode` 节点订阅层 | **需要新增，但落点改到节点层**（探针 1 结论） |
| 2 | `.flowItem { content-visibility:auto; contain-intrinsic-size:auto 96px }` | 官方无 | **需要新增**（探针 2 方案） |
| 3 | `:has()` → `data-compaction-has-body` 属性选择器（`.compactionRow:has(.compactionBody)`） | 官方仍有 `:has(.Sixlwa_compactionBody)`；上游 `chat_follow_anchor.rs` 只处理**滚动跟随**那一条 `:has`，与这条无关 | **上游未覆盖**，是否做留待 P2 实测（属于「折叠行 :has」热点，不在三件事范围） |
| 4 | `window.setTimeout` → `setTimeout`（1 处） | 官方产物里 `window.setTimeout` 仍有 2 处 | 废弃：收益不明、与性能目标无关 |
| 5 | `session-preload`（读 `session_projcache`、预折叠、内存缓存） | 官方无 | **P3 落地**（选型待 P3 前定） |
| 6 | esbuild 客户端构建脚本（`plugins/ui-chat/scripts/build-client.mjs`） | 不需要 | 废弃：我们只打补丁，不重打 bundle |
| 7 | `virtual.ts` 的 `computeVirtualWindow`（几何虚拟列表） | 官方无 | 废弃：旧分支自己也否掉了（绝对定位会破坏官方 `:nth-last-child` 与滚动锚点），改用 CSS `content-visibility` |

### 结论

真正的移植工作量只有 **#1（改落点）+ #2**，其余全部「上游已有 / 废弃」。
旧分支的 TSX 只作语义参考，不搬代码——我们写的是对产物 `lib/client.js` 的字符串补丁。

---

## 待拍板的问题（进 P2 前需要确认）

1. **补丁 A 的落点**：按探针结论改到节点订阅层（`useChatNode`），还是坚持旧实现的 entries 层？
   我的建议：先只做节点层（A1/A2），entries 层留作「结构性涌入」的第二步，实测不够再加。
2. **content-visibility 的 `contain-intrinsic-size`**：旧值 `auto 96px`。要不要按消息类型分档
   （工具卡/长回答/折叠行高度差异大）？建议先沿用 96px，用 P2 实测的滚动跳动数据再调。
3. **验收口径**：`MOD_PLAN §6` 的「冷启动 ≤5s」在 dev 构建下不可达（见 `docs/baseline-p0.md` §4），
   是否改为 release 构建口径？
4. **性能对照的基准从哪来**：`MOD_PLAN` 引用的 49–51 fps 是「旧机」数据，本机（2560×1440、
   当前 0.2.1-rc.2 干净核心）需要先测一条本机基线，否则改完没有可比对象。建议 P2 第一步就是
   在本机抓「万级 DOM 会话」基线，再动代码。
