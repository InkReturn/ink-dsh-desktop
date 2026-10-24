//! 流式渲染批量提交补丁（inkdsh P2 补丁 A）。
//!
//! 官方 `ChatSnapshotBuilder.apply()` 把每个流式增量分成 `structural`（新增/移动节点，
//! `order` 变化）和 `contentOnly`（同 key、内容变化）。纯文本流式输出走 `contentOnly`，
//! 此时 `entries`/`order` 引用不变，`ChatFlow` 根本不重渲染；真正逐 token 重渲染的是
//! `ChatNodeSeat` —— 它通过 `useChatNode(nodeKey)` 订阅单个节点的源，每个 token 换一次
//! 节点对象就重渲染一次（含整段 markdown 重解析）。
//!
//! 因此批量提交必须挂在**节点订阅层**，而不是 `entries` 数组层（旧分支挂在 entries 上，
//! 在纯文本流式下是空转）。这里把 `useChatNode(nodeKey)` 的返回值过一层 100ms 缓冲：
//! 只延迟「渲染快照」，不改 store、`order`、Turn 导航与滚动跟随的时序；最后一个值一定在
//! ≤100ms 内提交，终态不会丢。
//!
//! 失败语义与其它补丁一致：锚点失配 → 跳过并告警，绝不阻断启动。

use std::path::Path;

use crate::utils::{patch_core_file, patch_dsh, PatchOutcome};

const CLIENT: &str = "node_modules/@deepseek-ai/dsh-client-ui-chat/lib/client.js";

/// 提交间隔（毫秒）：10 次/秒足够流畅，把 token 级重渲染压到 1/3 ~ 1/6。
const BATCH_MS: &str = "100";

/// 节点读取语句（连上一行一起锚，确保落在 `ChatNodeSeat` 体内而不是别处同名调用）。
const ORIGINAL_NODE: &str = r#"			const bottom = useChatNodeBottom(nodeKey);
			const node = useChatNode(nodeKey);"#;
const PATCHED_NODE: &str = r#"			const bottom = useChatNodeBottom(nodeKey);
			const node = useBatchedChatNode(useChatNode(nodeKey));"#;

/// 组件声明处：在它之前插入缓冲实现（模块作用域，`react` 绑定与组件同一份）。
const ORIGINAL_SEAT: &str =
    r#"		const ChatNodeSeat = (0, react.memo)(function ChatNodeSeat({ nodeKey"#;
const PATCHED_SEAT: &str = r#"		//#region inkdsh/chat-stream-batch
		/**
		* 把一个节点的渲染快照压到每 {BATCH_MS}ms 最多一次（inkdsh 流式批量渲染）。
		*
		* 语义与 debounce 不同：定时器一旦启动就不因新值重置，持续流式时刷新频率恒定；
		* 最后一个值必在 {BATCH_MS}ms 内提交，终态不丢。
		*
		* @param node - `useChatNode(nodeKey)` 给出的最新节点对象。
		* @returns 最近一次已提交的节点对象。
		*/
		function useBatchedChatNode(node) {
			const [visible, setVisible] = (0, react.useState)(node);
			const slot = (0, react.useRef)(void 0);
			if (slot.current === void 0) slot.current = { timer: void 0, pending: void 0 };
			(0, react.useEffect)(() => {
				const state = slot.current;
				state.pending = node;
				if (state.timer !== void 0) return;
				state.timer = setTimeout(() => {
					state.timer = void 0;
					const next = state.pending;
					state.pending = void 0;
					if (next !== void 0) setVisible(next);
				}, {BATCH_MS});
			}, [node]);
			(0, react.useEffect)(() => () => {
				const state = slot.current;
				if (state.timer === void 0) return;
				clearTimeout(state.timer);
				state.timer = void 0;
				state.pending = void 0;
			}, []);
			return visible;
		}
		//#endregion
		const ChatNodeSeat = (0, react.memo)(function ChatNodeSeat({ nodeKey"#;

/// 插入用的缓冲实现（把间隔常量替换进去，便于调参与测试）。
fn patched_seat() -> String {
    PATCHED_SEAT.replace("{BATCH_MS}", BATCH_MS)
}

/// 对一份源码应用补丁：幂等、单次替换，任一处状态不明一律视为锚点失配。
fn patch_source(source: &str) -> PatchOutcome {
    let replacement = patched_seat();
    // 1. 已打过（或上游将来合并）就认账，不再改动。
    if source.matches(&replacement).count() == 1
        && source.matches(PATCHED_NODE).count() == 1
        && source.matches(ORIGINAL_NODE).count() == 0
    {
        return PatchOutcome::AlreadyPatched;
    }
    // 2. 两个锚点必须各出现一次；新旧并存或多份拷贝一律拒绝改。
    if source.matches(ORIGINAL_NODE).count() != 1
        || source.matches(ORIGINAL_SEAT).count() != 1
        || source.matches(&replacement).count() != 0
        || source.matches(PATCHED_NODE).count() != 0
    {
        return PatchOutcome::AnchorMissing;
    }
    // 3. 两处替换：先插实现，再改订阅点。
    let patched = source
        .replacen(ORIGINAL_SEAT, &replacement, 1)
        .replacen(ORIGINAL_NODE, PATCHED_NODE, 1);
    PatchOutcome::Patched(patched)
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

    /// 官方 0.2.1-rc.2 产物里两处锚点的原样片段（`test/fixtures/chat-stream-batch.txt`）。
    const FIXTURE: &str = include_str!("../../../../test/fixtures/chat-stream-batch.txt");

    #[test]
    fn patches_released_source_once() {
        let PatchOutcome::Patched(patched) = patch_source(FIXTURE) else {
            panic!("released source should patch");
        };
        assert!(patched.contains("function useBatchedChatNode("));
        assert!(patched.contains("useBatchedChatNode(useChatNode(nodeKey))"));
        assert!(!patched.contains(ORIGINAL_NODE));
        assert!(matches!(
            patch_source(&patched),
            PatchOutcome::AlreadyPatched
        ));
    }

    #[test]
    fn changed_node_read_is_skipped() {
        let source = FIXTURE.replace(
            "const node = useChatNode(nodeKey);",
            "const node = useChatNode2(nodeKey);",
        );
        assert!(matches!(patch_source(&source), PatchOutcome::AnchorMissing));
    }

    #[test]
    fn changed_component_declaration_is_skipped() {
        let source = FIXTURE.replace("function ChatNodeSeat({ nodeKey", "function ChatNodeSeat2({ nodeKey");
        assert!(matches!(patch_source(&source), PatchOutcome::AnchorMissing));
    }

    #[test]
    fn duplicated_anchors_are_skipped() {
        let source = format!("{FIXTURE}{FIXTURE}");
        assert!(matches!(patch_source(&source), PatchOutcome::AnchorMissing));
    }

    /// 补丁后的 JS 必须语法合法，且批缓冲行为符合契约（≤1 次/100ms、终态必达）。
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
            .join("../test/fixtures/chat-stream-batch-contract.mjs");
        let mut child = Command::new("node")
            .arg(script)
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("node is required for the batched-node JavaScript contract");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let output = child.wait_with_output().unwrap();
                assert!(
                    status.success(),
                    "batched-node contract failed: {status}
stdout: {}
stderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("batched-node contract timed out");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
