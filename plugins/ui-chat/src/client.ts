/**
 * `@inkdsh/ui-chat` 客户端入口。
 *
 * 由 DSH client loader 作为 `./client` 加载；注册虚拟列表视图后，官方 shell
 * 的输入框、设置页与会话头保持不变。
 */
import { register } from './register.js';

/** 客户端依赖的官方插件/服务。 */
export const inject = ['slots', 'sessions', 'uiConversation'];

/** 最小上下文形状（完整类型由官方包在 host 侧提供）。 */
interface PluginContext {
  slots?: unknown;
  sessions?: unknown;
}

/**
 * 客户端插件入口。
 *
 * @param ctx Cordis 客户端上下文。
 */
export function apply(ctx: PluginContext): void {
  if (!register(ctx as never)) {
    console.warn(
      '[inkdsh/ui-chat] conversation.view 插槽不可用，已跳过虚拟视图注册',
    );
  }
}
