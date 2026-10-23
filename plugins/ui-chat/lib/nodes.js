/**
 * 官方 ChatSnapshot 的最小结构投影。
 *
 * 官方节点是 target-owned JSON；这里只读取渲染必需字段，未知节点一律降级
 * 成可展开的文本，避免版本升级时白屏。
 */
/** 空快照常量，避免无 chat 来源时反复分配。 */
export const EMPTY_CHAT_SNAPSHOT = { nodes: [] };
/** 判断节点是否为工具调用。 */
export function isToolNode(node) {
    return node.kind.includes('tool');
}
/**
 * 从节点数据中提取可读文本。
 *
 * @param node 会话节点。
 * @returns 用户/助手文本；工具节点返回简要调用描述；未知结构返回截断 JSON。
 */
export function nodeText(node) {
    const data = node.data;
    if (typeof data === 'string')
        return data;
    if (typeof data !== 'object' || data === null)
        return '';
    const record = data;
    for (const key of ['text', 'content', 'val', 'title', 'summary']) {
        const value = record[key];
        if (typeof value === 'string')
            return value;
    }
    // 官方 assistant 可能把内容放在 blocks 数组里。
    const blocks = record.blocks;
    if (Array.isArray(blocks)) {
        const parts = blocks
            .map((block) => {
            if (typeof block === 'string')
                return block;
            if (typeof block === 'object' && block !== null) {
                const text = block.text;
                return typeof text === 'string' ? text : '';
            }
            return '';
        })
            .filter(Boolean);
        if (parts.length > 0)
            return parts.join('\n');
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
export function nodeLabel(node) {
    if (node.kind.includes('user'))
        return '你';
    if (node.kind.includes('assistant'))
        return 'DSH';
    if (isToolNode(node))
        return '工具';
    if (node.kind.includes('system'))
        return '系统';
    return '消息';
}
/** 稳定 key；官方缺省时退化为 kind+id。 */
export function nodeKey(node, index) {
    return node.key ?? `${node.kind}:${node.id ?? index}`;
}
/** 安全序列化未知对象；循环引用或过长时截断。 */
function safeJson(value) {
    try {
        const text = JSON.stringify(value);
        return text.length > 240 ? `${text.slice(0, 240)}…` : text;
    }
    catch {
        return String(value);
    }
}
