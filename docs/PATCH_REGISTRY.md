# inkdsh 补丁登记表（P3b 工作区，未提交）

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

## P3b 工作区：直连 Host 与静态资源磁盘缓存（2026-10-25）

> 这段是**运行时扩展**，不是 `patch_core_file` 的锚点补丁，不计入 `apply_all_at` 的 18 项。
> 当前仅在 `feat/inkdsh-mods` 工作区，未提交、未推送；debug 版已真机验收，release 未安装。

| 功能 | 位置 | 键/边界 | 失败行为 | 自动化证据 |
|---|---|---|---|---|
| LAN HTTP / 公网 HTTPS 直连 Host | `dsh-tauri-remote/src/host/service/direct.ts`、`machine.ts`、`client/components/machines-section.tsx` | 只存 origin，禁止路径/query/userinfo；访问令牌仅保存于远程配置，绝不向前端返回 | 连接失败不污染其它 Host；过期连接 generation 失效 | `direct.test.ts` 7 条；真实 `direct.integration.test.ts` |
| HTTPS gateway | `gateway.ts`、`gateway.utils.ts` | HTTP/HTTPS 分流；上游 Host/SNI/Origin；mint URL 同源；证书校验不放松 | 保持原网络/鉴权错误，不重定向 mint | gateway 两个测试文件 238 条，含真实本地 TLS、Cookie、Upgrade |
| 静态缓存写侧 | `static-cache.ts`，由 gateway 在已鉴权响应上调用 | 核心 `/assets/<file>`、favicon、manifest，以及带 `rev` query 的 `/plugins/??` 合并 bundle；拒绝 HTML/API/401/Set-Cookie/private/no-store/Vary:Cookie；单文件 64MB；完整响应后异步原子落盘 | 任何收集/读写/LRU 失败都不影响客户端响应 | `static-cache.test.ts` 5 条：完整、双 Host、中断、鉴权排除、插件 key |
| 静态缓存读侧 | `src-tauri/src/desktop/remote_cache.rs`、`frontend_preload.rs` | `<base>/inkdsh/remote-cache/<hostId>/<sha256(上游身份)前16位>/<sha256(path+query)>`；绑定远端即清空本机 preload 的 `frontend`；拦截器覆盖 `/plugins/*` | 读不到/损坏/hostId 异常/origin 不匹配均放行 gateway | Rust 缓存 5 条；真机两 Host `/assets/*` 与 `/plugins/` 均 hit=true |
| 后台预加载（P3c） | `prefetch.ts` + `machine.ts` + `gateway.ts` | 连接后 5s 解析首页静态引用；已有缓存跳过；串行 100ms 间隔；`x-inkdsh-prefetch` 不计入用户活动，用户请求活跃时暂停 | 任一请求/解析失败只终止本轮，不影响 Host 与页面 | `prefetch.test.ts` 2 条 + `prefetch.integration.test.ts` 2 条（落盘、二次跳过、活跃请求暂停） |
| 多 Host 切换绑定 | `src/hooks/use-remote.ts` + `bind_remote_frontend` + `webview.tsx` | 每窗口 `(hostId, gateway origin, 上游身份)`；保活帧常驻布局，`visibility` 切换（不用 `display:none`，避免 WebView2 暂停合成）；同 Host 重连会更新 gateway URL | 无绑定即禁用缓存，不影响切换 | `use-remote` + remote-poll 28 条；切换相关 46 条；真机 A→B→A 51–88ms |

### 明确未完成 / 下一次先做

- 已在真实 `192.168.31.83:3080`、`dshs.inkmo.top` 完成 `dsh-web-login` 密码登录与 A→B→A 切换；登录状态保持，切换 51–88ms。首连 LAN 5.4s、Inkmo 20.2s（远端插件 boot 本身约 20s）。
- **不缓存 HTML**，因此任务书 P3d 的「断网可渲染」未实现；P3c 后台预加载/闲时暂停已完成，二次访问 LAN 113ms、Inkmo 716ms（点击到 ready）。
- P3c 仅在 Host 连接成功后触发，应用启动不会自动重连未连接 Host；首连慢仍受远端插件 boot 限制（Inkmo 约 20s）。
- 远程插件/dist 需要重新构建并重启桌面才生效；源码与 build 通过不代表本机正在运行的实例已热更新。
- 全量前端单测：5408 passed / 18 failed / 5 skipped；修复新引入的 3 条 mock 污染后，相关 28 条通过。其余 15 条为 Windows symlink 权限与上游既有 fixture 问题（需复测基线登记）。
- 全量 Rust：1225 passed / 1 failed / 1 ignored；失败为 `config::proxy::tests::test_treats_an_empty_proxy_as_a_direct_connection` 外网探测 502，单跑亦 502；新增缓存测试 5/5、预加载 4/4、desktop 73/73 通过。

## 已知问题 / 观察项

| 项 | 数值 | 处置 |
|---|---|---|
| `contain-intrinsic-size: auto 96px` 估算误差 | 2000 条密集消息：估算 217,278px vs 真实 1,376,294px（16%） | 按拍板沿用 96px、不分档；拖滚动条单次可触发 ~21k px 重算，登记为观察项，不进验收门槛 |
| `dsh-plugin-ide` 宿主半激活失败 | `cannot get property "webServer" without inject` | 与本 fork 无关（第三方插件），安装记录见 `docs/PLUGIN_INSTALL_LOCAL.md` |
| clippy 既有 21 条告警 | 见 `docs/lint-debt.md` | 上游既有，不动；验收口径 = 我们改的文件零告警 |
