//! 屏外消息渲染跳过补丁（inkdsh P2 补丁 B）。
//!
//! 官方把 chat 的 CSS 以 `<style data-plugin-css="@deepseek-ai/dsh-client-ui-chat/ChatView.module.css">`
//! 的形式在运行时注入，类名来自 CSS Modules 哈希（当前是 `.EvIC1a_flowItem`）。哈希随核心
//! 版本变化，所以这里**不碰原 CSS 文本、也不硬编码类名**，而是在映射对象
//! `ChatView_module_css_default` 之后追加一条独立 `<style>`，选择器用映射值拼出来：
//! `.${ChatView_module_css_default.flowItem}{content-visibility:auto;contain-intrinsic-size:auto 96px}`。
//!
//! ## 为什么收益只能在 fixture 里量
//!
//! 这条补丁改的是**布局/绘制**成本，而应用内会话把完成的轮次折叠成 group 行：一个 60 轮
//! 会话只有约 100 个 `flowItem`、约 3.5k DOM（本机实测），本机（165Hz 屏）在这个规模下
//! 任何补丁都是满帧，读数没有鉴别力。真正能体现收益的是「万级 DOM 且不折叠」的形态，
//! 只有 fixture（`test/perf/make-fixture.mjs`，真实 CSS + 真实 DOM 形状，Edge 155 =
//! 本机 WebView2 155 同引擎）能稳定复现：70k DOM 时滚动期主线程占用 0.031 → 0.002，
//! 流式 30Hz 下 103.5 → 165 fps。详见 `docs/perf/baseline-local.md`。
//!
//! ## 与补丁 A 的唯一互扰点
//!
//! 流式期间新消息项是**动态插入**的，`content-visibility: auto` 对动态插入节点在个别
//! WebView2 版本上会出现「先白一帧再渲染」。补丁 A 压的正是流式提交频率，两者叠加时这类
//! 问题最难定位，所以 `test/perf/cv-effects.mjs --mode stream-insert` 专门守这条：
//! 插一条新消息后逐帧检查它是否立刻有真实高度（>50px）、是否落在可视区、滚动位置漂移多少。
//!
//! 失败语义与其它补丁一致：锚点失配 → 跳过并告警，绝不阻断启动。

use std::path::Path;

use crate::utils::{patch_core_file, patch_dsh, PatchOutcome};

const CLIENT: &str = "node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js";

/// 估算高度：沿用旧方案实测值，暂不按消息类型分档（分档需要真实高度分布数据）。
const INTRINSIC_SIZE: &str = "auto 96px";

/// 类名映射对象的末尾：借用它拿到哈希无关的 `flowItem` 类名。
const ORIGINAL_TAIL: &str = r#"			"turnSpacer": "EvIC1a_turnSpacer"
		};"#;

const PATCHED_TAIL: &str = r#"			"turnSpacer": "EvIC1a_turnSpacer"
		};
		//#region inkdsh/chat-content-visibility
		/**
		* 屏幕外的消息行跳过布局与绘制（保留 DOM，维持官方 scroll / Turn 导航与
		* `:nth-last-child` 结构）。类名取自 CSS Modules 映射对象，核心升级换哈希后仍然有效。
		*
		* 收益量法见模块注释：应用内到不了瓶颈区，只能靠 `test/perf/` 的万级 DOM fixture 对比；
		* `contain-intrinsic-size` 的估算误差会体现在拖动滚动条的位置偏移上，属已知观察项，
		* 不进验收门槛。
		*/
		if (typeof document !== "undefined" && document.getElementById("inkdsh-content-visibility") === null) {
			const inkdshContentVisibility = document.createElement("style");
			inkdshContentVisibility.id = "inkdsh-content-visibility";
			inkdshContentVisibility.textContent = "." + ChatView_module_css_default.flowItem + "{content-visibility:auto;contain-intrinsic-size:{INTRINSIC_SIZE}}";
			document.head.appendChild(inkdshContentVisibility);
		}
		//#endregion"#;

/// 生成插入文本（把估算高度替换进去，便于调参与测试）。
fn patched_tail() -> String {
    PATCHED_TAIL.replace("{INTRINSIC_SIZE}", INTRINSIC_SIZE)
}

/// 对一份源码应用补丁：幂等、单次替换，状态不明一律视为锚点失配。
fn patch_source(source: &str) -> PatchOutcome {
    let replacement = patched_tail();
    // 1. 已打过（或上游将来自带）就认账。
    if source.matches(&replacement).count() == 1 && source.matches(ORIGINAL_TAIL).count() == 1 {
        return PatchOutcome::AlreadyPatched;
    }
    // 2. 锚点必须唯一，且插入点没有被别的东西占过。
    if source.matches(ORIGINAL_TAIL).count() != 1 || source.matches(&replacement).count() != 0 {
        return PatchOutcome::AnchorMissing;
    }
    // 3. 只在映射对象之后追加，不改动类名与官方 CSS 文本。
    PatchOutcome::Patched(source.replacen(ORIGINAL_TAIL, &replacement, 1))
}

pub fn apply_at(core_dir: &Path) -> Result<(), String> {
    patch_core_file(core_dir, CLIENT, patch_source)
}

pub fn apply(app_handle: &tauri::AppHandle) -> Result<(), String> {
    patch_dsh(app_handle, CLIENT, patch_source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const FIXTURE: &str = include_str!("../../../../test/fixtures/chat-content-visibility.txt");

    #[test]
    fn patches_released_source_once() {
        let PatchOutcome::Patched(patched) = patch_source(FIXTURE) else {
            panic!("released source should patch");
        };
        assert!(patched.contains("inkdsh-content-visibility"));
        assert!(patched.contains("ChatView_module_css_default.flowItem"));
        assert!(patched.contains("content-visibility:auto;contain-intrinsic-size:auto 96px"));
        assert!(matches!(
            patch_source(&patched),
            PatchOutcome::AlreadyPatched
        ));
    }

    #[test]
    fn changed_class_map_tail_is_skipped() {
        let source = FIXTURE.replace("\"turnSpacer\"", "\"turnSpacerRenamed\"");
        assert!(matches!(patch_source(&source), PatchOutcome::AnchorMissing));
    }

    #[test]
    fn duplicated_anchors_are_skipped() {
        let source = format!("{FIXTURE}{FIXTURE}");
        assert!(matches!(patch_source(&source), PatchOutcome::AnchorMissing));
    }

    /// 补丁后的 JS 必须语法合法，且注入的样式文本与我们声明的完全一致。
    #[test]
    fn patched_core_passes_the_javascript_contract() {
        let core = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let target = core.path().join(CLIENT);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let PatchOutcome::Patched(patched) = patch_source(FIXTURE) else {
            panic!("fixture should patch");
        };
        std::fs::write(&target, &patched).unwrap();

        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test/fixtures/chat-content-visibility-contract.mjs");
        let mut child = Command::new("node")
            .arg(script)
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("node is required for the content-visibility contract");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    status.success(),
                    "content-visibility contract failed: {status}\nstdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("content-visibility contract timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
