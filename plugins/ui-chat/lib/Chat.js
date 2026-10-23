import { jsx as _jsx } from "react/jsx-runtime";
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { MessageList } from './MessageList.js';
import { EMPTY_CHAT_SNAPSHOT, } from './nodes.js';
import { BatchBuffer } from './virtual.js';
/** 无来源时的空订阅。 */
const noopSubscribe = () => () => undefined;
/** 无来源时的空快照读取。 */
const emptySnapshot = () => EMPTY_CHAT_SNAPSHOT;
/**
 * 虚拟列表 Chat 视图。
 *
 * 官方 shell 负责输入框、会话头与设置；本组件只接管 transcript 渲染。
 * 数据来自官方 chat target 快照，流式节点更新经 100ms 批量缓冲后进入列表。
 */
export function Chat({ chat, sessionId }) {
    const snapshot = useSyncExternalStore(chat?.subscribe ?? noopSubscribe, chat?.getSnapshot ?? emptySnapshot, chat?.getSnapshot ?? emptySnapshot);
    const nodes = useBatchedNodes(snapshot.nodes ?? []);
    return (_jsx("div", { className: "inkdsh-chat", "data-session": sessionId ?? '', children: _jsx(MessageList, { nodes: nodes }) }));
}
/**
 * 100ms 批量更新节点。
 *
 * 定时器由首个新值启动，后续 token 只覆盖 pending，不重置计时，因此持续
 * 流式输出时刷新频率稳定在 10Hz。
 */
function useBatchedNodes(nodes) {
    const [value, setValue] = useState(nodes);
    const buffer = useRef(null);
    if (buffer.current === null) {
        buffer.current = new BatchBuffer((next) => setValue(next), 100);
    }
    useEffect(() => {
        buffer.current?.push(nodes);
    }, [nodes]);
    useEffect(() => () => {
        buffer.current?.flushNow();
        buffer.current?.dispose();
    }, []);
    return value;
}
