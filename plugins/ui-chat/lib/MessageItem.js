import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { Suspense, lazy } from 'react';
import { nodeLabel, nodeText } from './nodes.js';
/** 工具卡片按需加载，首屏只保留占位文本。 */
const ToolCard = lazy(() => import('./ToolCard.js'));
/**
 * 渲染一条消息。
 *
 * 用户/助手/系统节点直接显示文本；工具节点进入懒加载卡片，避免首屏加载
 * 工具渲染依赖。
 */
export function MessageItem({ node }) {
    const text = nodeText(node);
    return (_jsxs("article", { className: "inkdsh-message", "data-kind": node.kind, children: [_jsx("header", { className: "inkdsh-message__label", children: nodeLabel(node) }), node.kind.includes('tool') || node.kind.includes('process') ? (_jsx(Suspense, { fallback: _jsx("div", { className: "inkdsh-message__text", children: text || '工具调用' }), children: _jsx(ToolCard, { node: node }) })) : (_jsx("div", { className: "inkdsh-message__text", children: text }))] }));
}
