/**
 * 官方 ChatSnapshot 的最小结构投影。
 *
 * 官方节点是 target-owned JSON；这里只读取渲染必需字段，未知节点一律降级
 * 成可展开的文本，避免版本升级时白屏。
 */

/** 一条会话节点（来自 `binding.target('chat')`）。 */
export interface ChatNodeLike {
  /** 引擎生成的稳定 key。 */
  readonly key?: string;
  /** 节点种类，如 `message.user` / `message.assistant` / `tool`。 */
  readonly kind: string;
  /** 业务 id。 */
  readonly id?: string;
  /** 节点数据；结构由官方定义，按需读取。 */
  readonly data?: unknown;
}

/** 官方 ChatSnapshot 的最小读取面。 */
export interface ChatSnapshotLike {
  /** 已物化的节点列表；缺省为空。 */
  readonly nodes?: readonly ChatNodeLike[];
}

/** 订阅式快照来源（官方 `ObservableSnapshot` 的结构化子集）。 */
export interface ObservableLike<T> {
  /** 读取当前快照。 */
  getSnapshot(): T;
  /** 订阅失效通知。 */
  subscribe(listener: () => void): () => void;
}

/** 空快照常量，避免无 chat 来源时反复分配。 */
export const EMPTY_CHAT_SNAPSHOT: ChatSnapshotLike = { nodes: [] };

/** 判断节点是否为工具调用。 */
export function isToolNode(node: ChatNodeLike): boolean {
  return node.kind.includes('tool');
}

/**
 * 从节点数据中提取可读文本。
 *
 * @param node 会话节点。
 * @returns 用户/助手文本；工具节点返回简要调用描述；未知结构返回截断 JSON。
 */
export function nodeText(node: ChatNodeLike): string {
  const data = node.data;
  if (typeof data === 'string') return data;
  if (typeof data !== 'object' || data === null) return '';

  const record = data as Record<string, unknown>;
  for (const key of ['text', 'content', 'val', 'title', 'summary'] as const) {
    const value = record[key];
    if (typeof value === 'string') return value;
  }

  // 官方 assistant 可能把内容放在 blocks 数组里。
  const blocks = record.blocks;
  if (Array.isArray(blocks)) {
    const parts = blocks
      .map((block) => {
        if (typeof block === 'string') return block;
        if (typeof block === 'object' && block !== null) {
          const text = (block as Record<string, unknown>).text;
          return typeof text === 'string' ? text : '';
        }
        return '';
      })
      .filter(Boolean);
    if (parts.length > 0) return parts.join('\n');
  }

  // 工具节点：尽量给出 name/input 摘要。
  const name = record.name ?? record.toolName;
  if (typeof name === 'string') {
    const input = record.input ?? record.args;
    return input === undefined ? name : `${name} ${safeJson(input)}`;
  }

  return safeJson(data);
}

/** 节点展示标签。 */
export function nodeLabel(node: ChatNodeLike): string {
  if (node.kind.includes('user')) return '你';
  if (node.kind.includes('assistant')) return 'DSH';
  if (isToolNode(node)) return '工具';
  if (node.kind.includes('system')) return '系统';
  return '消息';
}

/** 稳定 key；官方缺省时退化为 kind+id。 */
export function nodeKey(node: ChatNodeLike, index: number): string {
  return node.key ?? `${node.kind}:${node.id ?? index}`;
}

/** 安全序列化未知对象；循环引用或过长时截断。 */
function safeJson(value: unknown): string {
  try {
    const text = JSON.stringify(value);
    return text.length > 240 ? `${text.slice(0, 240)}…` : text;
  } catch {
    return String(value);
  }
}
