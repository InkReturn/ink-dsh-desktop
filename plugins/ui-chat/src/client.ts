/**
 * `@inkdsh/ui-chat` 客户端入口。
 *
 * 直接复用从官方 ui-chat 0.2.1-rc.2 复制并魔改的 `apply`：注册 Chat target、
 * 节点 renderers、stats、设置页与 conversation.view 插槽。inkdsh 只替换
 * 渲染层（批量更新与 flow 渲染隔离），业务逻辑保持官方实现。
 */
export { apply, inject } from './official/client/index.js';
