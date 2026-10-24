# 本机插件清单（2026-10-24 实测，`~/.dsh/profiles/web/package.json`）

按来源分四类。**决定那边能不能装的关键是"来源"那一列**：`link:` / `file:` 指向本地路径，新机器上没有这些路径，要么先把源文件带过去，要么在 npm 找原版替代。

## A. 本地 link（源码目录，新机器默认没有）

| 插件 | 来源路径 | 作用 |
|---|---|---|
| `@local/dsh-plugin-ide` | `D:\Dev\dsh-portable\payload\local-plugins\dsh-plugin-ide` | IDE 面板 |
| `@local/dsh-plugin-worktree` | 同上 `dsh-plugin-worktree` | worktree 管理 |
| `@local/dsh-usage-ledger` | 同上 `dsh-usage-ledger` | 用量账本 |
| `@local/dsh-host-monitor-lan` | 同上 `dsh-host-monitor-lan` | 局域网主机监控 |
| `@local/dsh-active-workspaces` | 同上 `dsh-active-workspaces` | 活跃工作区 |
| `@local/dsh-tool-session-title` | 同上 `dsh-tool-session-title` | 会话标题生成 |
| `@local/dsh-web-login` | 同上 `dsh-web-login` | 登录 |
| `dsh-antigravity` | 同上 `dsh-antigravity` | antigravity 集成 |
| `@local/dsh-model-picker-live` | 同上 `dsh-model-picker-live` | 模型选择器实时数据 |

## B. 本地 file: tgz（打包产物，新机器默认没有）

| 插件 | 文件 |
|---|---|
| `dsh-lan-access` | `vendor\dsh-lan-access-0.1.3.patched.tgz`（打过补丁） |
| `dsh-subagent-profile` | `vendor\dsh-subagent-profile-0.5.1.patched.tgz`（打过补丁） |
| `dsh-model-visibility` | `vendor\dsh-model-visibility-0.1.2.patched.tgz`（打过补丁） |
| `dsh-frosted-window` | `vendor\dsh-frosted-window-0.1.0.tgz` |
| `dsh-digipet` | `vendor\dsh-digipet-0.2.0.tgz` |
| `gal-view` | `vendor\gal-view-0.3.3.tgz` |
| `@liustack/modlens` | `vendor\liustack-modlens-3.26.0.patched.tgz`（打过补丁） |

## C. 本机 compat 目录（`~/.dsh/plugins/compat-020-20261002`，也是本地路径）

| 插件 | 说明 |
|---|---|
| `dsh-client-ui-aqua` | aqua 皮肤（**性能主犯**，但你在用） |
| `aegis` | Aegis 方法包 |
| `dsh-model-picker` | 模型选择器 |
| `dsh-lan-compat` | 局域网兼容 |
| `dsh-web-search-tavily` | Tavily 搜索 |

## D. npm 原版（那边直接装就行）

| 插件 | 版本 |
|---|---|
| `dsh-context` | `0.54.4` |
| `dsh-skin-market` | `^0.1.47` |
| `@deepseek-ai/dsh-settings` | `0.2.1-rc.2`（官方） |

## 给那边的建议

- D 类直接装，无成本。
- A/B/C 三类共 **19 个包**，源都在本机（`D:\Dev\dsh-portable\payload\` 两个目录 + `C:\Users\Administrator\.dsh\plugins\compat-020-20261002\`）。整体打包发过去最省事，不用一个个找 npm 对应版本（有些还是打过补丁的，npm 上没有）。
- 配置（模型路由、压缩策略、权限预设）在 `~/.dsh\profiles\web\cordis.patch.yml`，这个文件**别照抄**——里面有这台机器专属的 provider 配置和密钥引用，只搬插件清单部分，配置让那边自己重建。
