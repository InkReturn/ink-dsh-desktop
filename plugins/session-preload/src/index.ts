/**
 * `@inkdsh/session-preload` 插件入口。
 *
 * 目标：在 Cordis 服务就绪后，把最近 10 个会话的投影折叠进内存，减少客户端
 * 首屏逐会话冷读日志的次数。
 *
 * 说明：当前 Cordis 版本没有通用 `ctx.on('ready')`；`inject` 声明 + 依赖注入
 * 回调本身就是“服务就绪”边界。这里在 `apply` 中立即启动异步预折叠，不阻塞
 * 插件加载，并在插件卸载时清空缓存。
 */
import type { Context } from '@deepseek-ai/cordis';
import type {} from '@deepseek-ai/dsh-session';
import type {} from '@deepseek-ai/dsh-session-projection';

import {
  preloadRecentSessions,
  resolveDshHome,
  SessionPreloadCache,
  type PreloadStats,
} from './preload.js';

/** 本插件依赖的 Cordis 服务。 */
export const inject = ['sessionProjections', 'sessions'];

/** 启动时最多预折叠的会话数。 */
const DEFAULT_LIMIT = 10;

/**
 * 启动会话预折叠。
 *
 * @param ctx 已注入 `sessionProjections` 与 `sessions` 的 Cordis 上下文。
 */
export function apply(ctx: Context): void {
  const cache = new SessionPreloadCache();

  // 1. 绑定生命周期：卸载时清空内存缓存，避免泄漏会话投影。
  ctx.effect(
    () => {
      let disposed = false;
      const home = resolveDshHome();
      void preloadRecentSessions(
        ctx.sessions,
        ctx.sessionProjections,
        home,
        cache,
        DEFAULT_LIMIT,
      )
        .then((stats: PreloadStats) => {
          if (disposed) return;
          ctx.logger.info(
            `inkdsh session preload: ${stats.count} session(s), ${stats.live} live, dir=${stats.directory}`,
          );
        })
        .catch((error: unknown) => {
          if (disposed) return;
          ctx.logger.warn(`inkdsh session preload failed: ${String(error)}`);
        });

      return () => {
        disposed = true;
        cache.clear();
      };
    },
    'inkdsh.sessionPreload',
  );
}
