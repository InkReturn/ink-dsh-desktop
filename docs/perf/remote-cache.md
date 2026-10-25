# P3b/P3c 远程 Host 登录、缓存与预加载实测（2026-10-26）

## 已验证范围

- 直连 Host：`http://192.168.31.83:3080/`（LAN HTTP）与 `https://dshs.inkmo.top/`（公网 HTTPS）。
- 两者均由远端 `dsh-web-login` 校验登录凭据并签发会话 Cookie；桌面只按 Host 代持凭据、调用登录流程，不复制密码校验。
- Host 编辑支持选择「密码登录（dsh-web-login）/ 访问令牌」；登录用户名和路径可配置，默认 `/__login`。
- 切换器默认只显示手动添加 Host；`~/.ssh/config` 自动发现项仍留在「管理机器」，不再混入日常切换列表。
- A→B→A→B 已实测；保活页面常驻布局，只切换 `visibility` 与指针事件，不销毁/重建 iframe；本机 + 两个 Host 的 iframe 总数稳定为 3。
- 远端首次加载用 `dsh://plugin-boot:ready` 结束加载遮罩；若远端页面有挂起的外部资源导致 iframe `onLoad` 永不触发，20s 兜底强制揭开。

## 实测数字

| 场景 | 结果 |
|---|---|
| LAN → Inkmo → LAN → Inkmo | **51–88ms**（4 次可见 iframe 切换） |
| 切换后 150ms 截图 | 页面已完整渲染；无加载遮罩、无本机/远端叠加 |
| 首连 LAN（核心 assets 已缓存、插件 bundle 首次） | **3.2s** 到插件 boot ready |
| 首连 Inkmo（核心 assets 已缓存、插件 bundle 首次） | **20.4s** 到插件 boot ready；远端插件 boot 本身约 20s |
| 二次访问 LAN（含插件 bundle 缓存） | 点击到目标 iframe **113ms**，即刻 ready |
| 二次访问 Inkmo（含插件 bundle 缓存） | 点击到目标 iframe **219ms**，iframe 到 ready **497ms**，点击到 ready **716ms** |
| 页面 iframe 数量 | 本机模式 **3**（本机 + 两个保活 Host）；远端模式 **2**（两个 Host），本地 iframe 卸载 |
| 登录状态 | 两个 Host 均保持 `connected`，切换不要求重新输入密码 |
| `dsh-web-login` 实际认证 | 303 + 会话 Cookie；随后 gateway 页面 200、静态资源 200 |
| Host 切换器列表 | 仅 `LAN 192.168.31.83`、`Inkmo`，无 SSH 自动发现候选 |

> 切换测量是壳层「可见 iframe / `visibility` 状态」的耗时，不包含首次登录和首次整页初始化；保活切换不再重复这些步骤。早期用 `display:none` 保活时 WebView2 会暂停非活动 iframe 的合成，切回第一帧只剩背景；改为 `visibility:hidden` 后 150ms 内完整呈现。

## 缓存与认证边界

- gateway 只在已鉴权、允许缓存的静态响应上写盘；HTML、登录页、API、`Set-Cookie`、`private/no-store`、压缩未解码响应不进入缓存。
- 目录：`<base>/inkdsh/remote-cache/<hostId>/<sha256(上游身份)前16位>/<sha256(path)>`。
- 已确认 LAN 与 Inkmo 各自缓存目录均有真实 bundle；改 Host 地址不会命中旧 origin 的缓存。
- 密码、访问令牌不出现在 iframe URL、缓存文件或 meta；日志不回显凭据。
- `X-Inkdsh-Cache: HIT` 只由 WebView2 原生拦截器添加，直接 curl gateway 不带此头，不能用 curl 判断原生命中；本次改用原生 debug 日志验证（见下）。
- 本机 preload 与远程缓存互斥：绑定远端 Host 时清空本机 preload 的 `frontend` 状态，否则本机 preload 曾成功会让远程缓存分支永远不可达；回到本地后由本机 iframe 重新走 preload/HTTP。
- `RUST_LOG=debug` 的 `inkdsh::remote_cache` 日志确认：LAN、Inkmo 首次加载各 4 个 `/assets/*` 均为 `hit=true`，HTML `/` 为 `hit=false`（按设计回源）。
- 插件 bundle 也进入缓存：`/plugins/??...&rev=...` 的 key 含完整 query；LAN 缓存体积约 **9.2MB + 5.3MB + 46KB**，Inkmo 约 9.2MB + 5.3MB。二次访问日志中 3 个 `path=/plugins/` 请求均 `hit=true`。
- 原生拦截器新增 `http://127.0.0.1:*/plugins/*` 过滤器；否则缓存文件存在但 WebView2 根本不会把插件 bundle 请求交给原生读侧。

## P3c 后台预加载

- 远端 Host 连接成功后，后台等待 5s，再抓取首页 HTML，解析同源 `/assets/*`、`/manifest.webmanifest`、favicon 和 `/plugins/??` 合并 bundle，交回 gateway tee 落盘。
- 已有完整缓存的文件直接跳过；串行拉取，每个文件间隔 100ms，避免与前台争带宽。
- 闲时判断：gateway 统计**非预加载**用户请求的并发数与最后活动时间；预加载请求带 `x-inkdsh-prefetch: 1`，不计入用户活动。用户请求仍在进行（含流式输出）时，预加载在文件之间暂停；进入空闲 2s 后才继续。
- 连接断开 / detach / give-up 会取消该 Host 的预加载。
- 集成测试覆盖：真实 gateway + HTML 静态引用 + 插件 bundle，验证落盘、二次跳过、以及「用户请求活跃时不会开始下一个资源」。

## 自动化证据

- 真实 core connection + `dsh-web-login`：密码登录 303，gateway 页面 200，静态资源 200。
- Host 状态机 / 切换器 / 保活 WebView 相关测试 **46/46**。
- P3 登录/网关/缓存/预加载/界面相关回归 **481/481**（11 files，含真实 HTTP 登录、缓存写盘与预加载暂停）。
- Rust `desktop::` **73/73**（含原生缓存读侧 5 条、远端绑定生命周期 1 条）；`pnpm typecheck`、变更文件 eslint、`pnpm build` 通过。

## 尚未完成

- P3d 断网可渲染/离线提示未做；当前 HTML 不缓存，不声称离线可用。
- P3c 目前由「连接成功」触发；应用启动时不会自动重连并预加载未连接的 Host（需要 P3c+ 或用户手动连接一次）。
- release 冷启动测量仍按用户要求跳过。
- 登录插件自身不改；认证失败会提示远端 `dsh-web-login` 的原因，不自动绕过。
