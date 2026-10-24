# inkdsh 补丁登记表（P2 完成后）

对应 `MOD_PLAN.md §3.2` 的补丁规范。上游基线 `47f03f4`（v0.25.0）/ 核心 `@deepseek-ai/dsh@0.2.1-rc.2`。
上游 sync 时按本表逐条核对锚点；任一条失配都只告警跳过，不阻断启动（`patch_core_file` 的 best-effort 通道）。

## 本 fork 追加的补丁

| 补丁 | 文件 | 目标 | 锚点（官方产物） | 失效行为 | 验收证据 |
|---|---|---|---|---|---|
| **A. 流式批量提交** | `src-tauri/src/service/patch/chat_stream_batch.rs` | `@deepseek-ai/dsh-client-ui-chat/lib/client.js` | ① `ChatNodeSeat` 体内的 `const bottom = useChatNodeBottom(nodeKey);` + `const node = useChatNode(nodeKey);`（连上一行一起锚）<br>② `const ChatNodeSeat = (0, react.memo)(function ChatNodeSeat({ nodeKey` | 锚点不唯一/缺失 → `AnchorMissing`，`log::warn!` | 5 条 Rust 测试 + `test/fixtures/chat-stream-batch-contract.mjs`（窗口合并、末值必达、500ms ≤6 次提交） |
| **B. 屏外渲染跳过** | `src-tauri/src/service/patch/chat_content_visibility.rs` | 同上 | 类名映射对象末尾：`"turnSpacer": "EvIC1a_turnSpacer"` + `};` | 同上 | 4 条 Rust 测试 + `test/fixtures/chat-content-visibility-contract.mjs`（注入文本/id/幂等/无 document 安全） |

挂点：`service/patch/mod.rs::apply_all_at`（16 → 18 项）与 `service/workflow/launch.rs`（逐个
`apply(&app_handle)`，失败只 `log::warn!`）。

### 锚点为什么这样选

- **A** 必须锚在 `ChatNodeSeat` 的节点订阅处：官方 `ChatSnapshotBuilder.apply()` 把流式增量分成
  `structural` / `contentOnly`，纯文本流式走 `contentOnly`（`entries`/`order` 不变），
  `ChatFlow` 不重渲染；逐 token 重渲染的是节点订阅。旧分支挂在 `entries` 层是空转。
- **B** 不锚哈希类名（`EvIC1a_` 随版本变），也不改官方 CSS 文本；改为在映射对象
  `ChatView_module_css_default` 之后追加独立 `<style>`，选择器用映射值拼出 → 哈希无关。

## 上游已有、我们直接采用的

| 能力 | 位置 | 说明 |
|---|---|---|
| `:has()` 滚动跟随 → 属性选择器 | `service/patch/chat_follow_anchor.rs` | 与旧方案重叠，不重做 |
| 滚动跟随降噪（reduced-motion） | `service/patch/scroll_follow.rs` | 直接使用 |
| 其余 14 个官方文件补丁 | `service/patch/*.rs` | 原样保留 |

## 已知问题 / 观察项

| 项 | 数值 | 处置 |
|---|---|---|
| `contain-intrinsic-size: auto 96px` 估算误差 | 2000 条密集消息：估算 217,278px vs 真实 1,376,294px（16%） | 按拍板沿用 96px、不分档；拖滚动条单次可触发 ~21k px 重算，登记为观察项，不进验收门槛 |
| `dsh-plugin-ide` 宿主半激活失败 | `cannot get property "webServer" without inject` | 与本 fork 无关（第三方插件），安装记录见 `docs/PLUGIN_INSTALL_LOCAL.md` |
| clippy 既有 21 条告警 | 见 `docs/lint-debt.md` | 上游既有，不动；验收口径 = 我们改的文件零告警 |
