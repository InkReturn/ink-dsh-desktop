import { jsx as _jsx } from "react/jsx-runtime";
import { useVirtualizer } from '@tanstack/react-virtual';
import { useRef } from 'react';
import { MessageItem } from './MessageItem.js';
import { nodeKey } from './nodes.js';
/** 单条消息的估算高度（px）；高度差异由 tanstack 动态测量修正。 */
const ESTIMATED_ITEM_HEIGHT = 96;
/** 可视区上下各多渲染的条数。 */
const OVERSCAN = 5;
/**
 * 只渲染可视区与上下缓冲的虚拟列表。
 *
 * 通过 `@tanstack/react-virtual` 测量真实高度；滚动容器独立，不依赖官方
 * chat 的滚动实现。
 */
export function MessageList({ nodes }) {
    const parentRef = useRef(null);
    const virtualizer = useVirtualizer({
        count: nodes.length,
        getScrollElement: () => parentRef.current,
        estimateSize: () => ESTIMATED_ITEM_HEIGHT,
        overscan: OVERSCAN,
        getItemKey: (index) => nodeKey(nodes[index] ?? { kind: 'unknown' }, index),
    });
    return (_jsx("div", { ref: parentRef, className: "inkdsh-transcript", "data-testid": "inkdsh-transcript", children: _jsx("div", { className: "inkdsh-transcript__spacer", style: { height: virtualizer.getTotalSize(), position: 'relative' }, children: virtualizer.getVirtualItems().map((item) => {
                const node = nodes[item.index];
                if (!node)
                    return null;
                return (_jsx("div", { "data-index": item.index, ref: virtualizer.measureElement, style: {
                        position: 'absolute',
                        top: 0,
                        left: 0,
                        width: '100%',
                        transform: `translateY(${item.start}px)`,
                    }, children: _jsx(MessageItem, { node: node }) }, item.key));
            }) }) }));
}
