import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { nodeText } from './nodes.js';
/**
 * 工具调用卡片（懒加载）。
 *
 * 仅展示工具名与参数摘要；完整的工具专属渲染仍由官方对应 renderer 承担，
 * 本组件只在虚拟视图里提供可读占位，避免引入整套工具 UI 依赖。
 */
export default function ToolCard({ node }) {
    return (_jsxs("div", { className: "inkdsh-tool-card", children: [_jsx("div", { className: "inkdsh-tool-card__title", children: "\u5DE5\u5177\u8C03\u7528" }), _jsx("pre", { className: "inkdsh-tool-card__body", children: nodeText(node) })] }));
}
