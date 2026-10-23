import { preloadRecentSessions, resolveDshHome, SessionPreloadCache, } from './preload.js';
/** 本插件依赖的 Cordis 服务。 */
export const inject = ['sessionProjections', 'sessions'];
/** 启动时最多预折叠的会话数。 */
const DEFAULT_LIMIT = 10;
/**
 * 启动会话预折叠。
 *
 * @param ctx 已注入 `sessionProjections` 与 `sessions` 的 Cordis 上下文。
 */
export function apply(ctx) {
    const cache = new SessionPreloadCache();
    // 1. 绑定生命周期：卸载时清空内存缓存，避免泄漏会话投影。
    ctx.effect(() => {
        let disposed = false;
        const home = resolveDshHome();
        void preloadRecentSessions(ctx.sessions, ctx.sessionProjections, home, cache, DEFAULT_LIMIT)
            .then((stats) => {
            if (disposed)
                return;
            ctx.logger.info(`inkdsh session preload: ${stats.count} session(s), ${stats.live} live, dir=${stats.directory}`);
        })
            .catch((error) => {
            if (disposed)
                return;
            ctx.logger.warn(`inkdsh session preload failed: ${String(error)}`);
        });
        return () => {
            disposed = true;
            cache.clear();
        };
    }, 'inkdsh.sessionPreload');
}
