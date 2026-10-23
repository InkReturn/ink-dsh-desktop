/**
 * 注册客户端视图到官方 conversation 插槽。
 *
 * 官方真实接口（0.2.1-rc.2）没有 `ctx.uiConversation.registerTarget`；等价入口是
 * `ctx.slots.inject('conversation.view', () => ctx.slots.register(...))`。这里注册
 * 一个独立视图 id（`inkdsh-chat`），读取官方 `chat` target 的已物化快照，不重复
 * 定义会话事件，因此不会与官方 chat 的解析逻辑冲突。
 */
import { Chat } from './Chat.js';
/**
 * 注册虚拟 Chat 视图。
 *
 * @param ctx Cordis 客户端上下文。
 * @returns 插槽可用并完成注册时为 true；服务缺失时为 false（插件降级为空操作）。
 */
export function register(ctx) {
    const slots = ctx.slots;
    if (!slots?.inject || !slots.register)
        return false;
    slots.inject('conversation.view', () => slots.register({
        name: 'conversation.view',
        id: 'inkdsh-chat',
        order: 10,
        label: () => 'inkdsh',
        children: {},
        inject: (sessionId) => {
            const binding = ctx.sessions?.binding(sessionId);
            return { chat: binding?.target('chat'), sessionId };
        },
    }, Chat));
    return true;
}
