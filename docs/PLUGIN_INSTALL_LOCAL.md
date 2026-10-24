# 本机插件安装记录（2026-10-24/25，Windows dev 环境）

按 `docs/inkdsh/PLUGIN_INVENTORY.md` 把 21 个插件装到桌面端。记录踩到的四个坑，换机器/重装时照这份走。

## 0. 装到哪个 profile（第一个坑）

桌面端**实际使用的是 `profiles/tauri`**，不是 `profiles/web`。
`dsh web` CLI 独立启动时才用 `web`。装错 profile 的现象：插件在 `web` 里能激活，
但桌面端重启后一个都看不到（客户端模块数还是 82）。

```bash
# 桌面端（debug 构建）：DSH_HOME=~/.dsh.dev，profile=tauri
dsh plugin --profile tauri add <spec…>
```

## 1. 三个包先解压到固定位置

`D:\inkdsh-plugins\local-plugins\`（9 个源码目录）、`vendor\`（7 个 tgz **再加** 7 个解压目录）、
`compat-020\`（5 个 compat 目录）。SHA256 按上传方给的清单核对。

## 2. tgz 不要用 `file:` 装（第二个坑）

`file:` 装进去的包会被桌面端的 `ensure_internal_plugins` 当作「孤儿内置预设插件」**自动卸载**
（日志：`Uninstalling orphaned internal preset plugins: [...]`，重启后这 7 个直接消失）。

正确做法：把 tgz 解压成目录，再用 `link:` 装。

```bash
for t in vendor/*.tgz; do
  out="vendor/$(basename "$t" .tgz)"; mkdir -p "$out"; tar -xzf "$t" -C "$out" --strip-components=1
done
dsh plugin --profile tauri add link:D:/inkdsh-plugins/vendor/<每个目录>
```

## 3. `link:` 的包要在自己的目录里装依赖（第三个坑）

`link:` 只做符号链接，不会装被链接包的依赖。现象：

- `dsh-plugin-worktree`：`Cannot find package 'zod'`
- `dsh-model-visibility`：`Cannot find package '@deepseek-ai/schemastery'`

```bash
cd D:/inkdsh-plugins/local-plugins/dsh-plugin-worktree && pnpm install
cd D:/inkdsh-plugins/vendor/dsh-model-visibility-0.1.2.patched && pnpm install
# 该包的 prepare 脚本会因缺 tsconfig.json 报错，忽略即可 —— 依赖已经装好就行
```

## 4. 版本门禁

`dsh-subagent-profile@0.5.1` 声明 `@deepseek-ai/dsh-* >=0.1.0-rc.6 <0.2.0`，与 dsh 0.2.1-rc.2 不符，
安装会被拒绝并整体回滚。按需放行（精确到版本，可撤销）：

```bash
dsh plugin --profile tauri allow-version dsh-subagent-profile@0.5.1 --dsh-version 0.2.1-rc.2 --accept-risk
```

## 5. 当前状态（2026-10-25）

| 项 | 结果 |
|---|---|
| profile `tauri` 依赖 | 35 个（14 个 dsh-tauri 内置 + 21 个本机插件） |
| 启动 | `STARTUP_READY: client modules 94/94` |
| 激活 | 20/21；失败 1 个：`@local/dsh-plugin-ide` → `cannot get property "webServer" without inject` |
| 额外修复 | `dsh-plugin-worktree`、`dsh-model-visibility` 的缺失依赖已就地 `pnpm install` |
| aqua | 已安装但**不挂载**（用户决定暂不启用；纯 UI 插件无 `dsh.bundle`，需要显式 `insert` 条目才会加载）。源码留在 `D:\inkdsh-plugins\compat-020\dsh-client-ui-aqua`，后面重搓皮肤时参考 |

## 6. 顺手记：磁盘

安装+构建期间 D: 盘两度写满（`os error 112`），Rust 链接失败、`tsdown --watch` 曾 OOM。
构建缓存清理后可用 `CARGO_INCREMENTAL=0` 规避增量目录（637MB）。D: 总容量 1.9T 已用满，重装前先腾空间。

## 7. 重启方式

```bash
cd D:/mydsh/ink-dsh-desktop
pnpm build:plugins          # 生成 packages/*/dist（必须先跑，否则 dsh-tauri-* 全部 import 失败）
CARGO_INCREMENTAL=0 pnpm exec tauri dev
```

> 直接用 `pnpm dev:desktop` 也行，但它会并行起 14 个 `tsdown --watch`，在内存/磁盘紧的时候容易 OOM 或写满盘。
