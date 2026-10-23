import { Suspense, lazy } from 'react';

import { nodeLabel, nodeText, type ChatNodeLike } from './nodes.js';

/** 工具卡片按需加载，首屏只保留占位文本。 */
const ToolCard = lazy(() => import('./ToolCard.js'));

/** 单条消息项属性。 */
export interface MessageItemProps {
  /** 官方会话节点。 */
  node: ChatNodeLike;
}

/**
 * 渲染一条消息。
 *
 * 用户/助手/系统节点直接显示文本；工具节点进入懒加载卡片，避免首屏加载
 * 工具渲染依赖。
 */
export function MessageItem({ node }: MessageItemProps) {
  const text = nodeText(node);
  return (
    <article className="inkdsh-message" data-kind={node.kind}>
      <header className="inkdsh-message__label">{nodeLabel(node)}</header>
      {node.kind.includes('tool') || node.kind.includes('process') ? (
        <Suspense fallback={<div className="inkdsh-message__text">{text || '工具调用'}</div>}>
          <ToolCard node={node} />
        </Suspense>
      ) : (
        <div className="inkdsh-message__text">{text}</div>
      )}
    </article>
  );
}
