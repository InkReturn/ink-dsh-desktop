import { nodeText, type ChatNodeLike } from './nodes.js';

/** 工具卡片属性。 */
export interface ToolCardProps {
  /** 官方工具节点。 */
  node: ChatNodeLike;
}

/**
 * 工具调用卡片（懒加载）。
 *
 * 仅展示工具名与参数摘要；完整的工具专属渲染仍由官方对应 renderer 承担，
 * 本组件只在虚拟视图里提供可读占位，避免引入整套工具 UI 依赖。
 */
export default function ToolCard({ node }: ToolCardProps) {
  return (
    <div className="inkdsh-tool-card">
      <div className="inkdsh-tool-card__title">工具调用</div>
      <pre className="inkdsh-tool-card__body">{nodeText(node)}</pre>
    </div>
  );
}
