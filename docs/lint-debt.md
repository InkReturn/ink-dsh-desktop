# Clippy 既有告警登记（上游债，不动）

- 登记日期：2026-10-24
- 工具链：`rustc 1.95.0` / `cargo clippy 1.95.0`
- 结论：**`cargo clippy -- -D warnings` 在本仓库当前无法通过**，21 条告警全部来自上游既有代码，
  与 inkdsh 魔改无关（`service/patch/*`、`workflow/launch.rs` 零告警）。

## 验收口径（本项目）

| 项 | 口径 |
|---|---|
| 硬门槛 | **我们改/新增的文件零告警** |
| 上游既有 21 条 | 登记在这里，标「既有」，不修、不加 `#[allow]`、不钉 clippy 版本 |
| 上游 sync 时 | 对照本表：哪条消失了＝上游自己修了；哪条新增＝上游又欠了新债，追加登记 |

检查我们自己的文件是否干净：

```bash
cd src-tauri
cargo clippy --lib 2>&1 | grep -E "chat_stream_batch|chat_content_visibility|patch/mod\.rs|workflow/launch\.rs"
# 期望：无输出
```

## 既有告警清单（21 条）

| # | 文件:行 | lint | 说明 |
| --- | --- | --- | --- |
| 1 | `src/service/workflow/process.rs:16` | `unused_imports` | `tauri::AppHandle` 未使用（Windows 专属分支调整后遗留） |
| 2 | `src/service/backup/recovery.rs:78` | `unused_mut` | `let mut builder` 不需要 `mut` |
| 3 | `src/desktop/builder.rs:675` | `dead_code` | `ViewMenuEntry` 的 `id`/`enabled`/`shortcut` 字段未被读取（菜单构造用别的路径） |
| 4 | `src/bridge/pet.rs:1553` | `nonminimal_bool` | 三重取反可合并，属可读性债 |
| 5 | `src/config/proxy.rs:165` | `redundant_closure` | `|key| std::env::var_os(key)` 可直接传函数 |
| 6 | `src/desktop/builder.rs:1940` | `collapsible_match` | `match` 分支里的 `if` 可折叠为守卫 |
| 7 | `src/service/core/local.rs:388` | `nonminimal_bool` | `!opt.is_some_and(..)` 可用 `is_none_or` |
| 8 | `src/service/data_dir/fs_ops.rs:113` | `manual_is_multiple_of` | `n % k == 0` → `n.is_multiple_of(k)` |
| 9 | `src/service/plugin/install/single.rs:738` | `doc_lazy_continuation` | 中文文档换行未缩进，被判定为续行 |
| 10 | `src/service/plugin/install/single.rs:739` | `doc_lazy_continuation` | 同上 |
| 11 | `src/service/plugin/install/single.rs:740` | `doc_lazy_continuation` | 同上 |
| 12 | `src/service/plugin/internal/mod.rs:682` | `needless_borrow` | `&profile` 已是引用 |
| 13 | `src/service/plugin/internal/mod.rs:683` | `needless_borrow` | 同上 |
| 14 | `src/service/plugin/internal/mod.rs:834` | `nonminimal_bool` | `!x.is_some_and(..)` 可用 `is_none_or` |
| 15 | `src/service/plugin/internal/mod.rs:870` | `needless_borrow` | `&presets` 已是引用 |
| 16 | `src/service/update/meta.rs:199` | `manual_pattern_char_comparison` | `matches!(c, '?'\|'#'\|'/')` 可换数组模式 |
| 17 | `src/service/update/meta.rs:219` | `manual_pattern_char_comparison` | 同上（4 个字符） |
| 18 | `src/service/workflow/early.rs:6` | `type_complexity` | `Mutex<Option<watch::Sender<Option<Result<(),String>>>>>` 建议提 type |
| 19 | `src/service/workflow/early.rs:152` | `redundant_closure` | `|prepared| setup(prepared)` → `setup` |
| 20 | `src/service/workflow/process.rs:443` | `manual_is_multiple_of` | 对齐检查可用 `is_multiple_of` |
| 21 | `src/service/workflow/process.rs:487` | `manual_c_str_literals` | `b"...\0"` 可用 `c"..."` 字面量（1.77+） |

> 说明：其中 9–11、20–21 是 **rustc 1.95 新增/收紧的 lint**（旧工具链下不报），
> 属「工具链升级带来的既有债」，不是代码退化。
