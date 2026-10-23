/**
 * 虚拟列表与增量渲染的纯逻辑工具。
 *
 * 这里不依赖 React，便于单元测试；组件层只负责把结果映射到 DOM。
 */
/**
 * 计算只渲染可视区及上下缓冲的窗口。
 *
 * @param input 滚动、可视区与消息数量。
 * @returns 起止索引与占位高度；滚动越界时收敛到合法范围。
 */
export function computeVirtualWindow(input) {
    const overscan = Math.max(0, Math.trunc(input.overscan ?? 5));
    const itemHeight = Math.max(1, input.itemHeight);
    const count = Math.max(0, Math.trunc(input.count));
    if (count === 0)
        return { start: 0, end: 0, offsetY: 0, totalHeight: 0 };
    const scrollTop = Math.max(0, input.scrollTop);
    const firstVisible = Math.floor(scrollTop / itemHeight);
    const visibleCount = Math.ceil(Math.max(0, input.viewportHeight) / itemHeight) + 1;
    const start = Math.max(0, firstVisible - overscan);
    const end = Math.min(count, firstVisible + visibleCount + overscan);
    return {
        start,
        end,
        offsetY: start * itemHeight,
        totalHeight: count * itemHeight,
    };
}
/** 默认调度器：浏览器/Node 的 setTimeout。 */
export const defaultScheduler = {
    set: (callback, delayMs) => setTimeout(callback, delayMs),
    clear: (handle) => clearTimeout(handle),
};
/**
 * 100ms 批量缓冲：流式 token 只更新“最新值”，每 100ms 最多提交一次。
 *
 * 与简单 debounce 不同，定时器一旦启动就不会被新值重置，因此持续流式输出
 * 时仍能保证稳定刷新频率。
 */
export class BatchBuffer {
    intervalMs;
    flush;
    scheduler;
    pending;
    timer;
    disposed = false;
    /**
     * @param flush 批次提交回调。
     * @param intervalMs 提交间隔；默认 100ms。
     * @param scheduler 定时器实现；默认 setTimeout。
     */
    constructor(flush, intervalMs = 100, scheduler = defaultScheduler) {
        this.flush = flush;
        this.intervalMs = intervalMs;
        this.scheduler = scheduler;
    }
    /** 写入最新值；存在未提交定时器时不重置它。 */
    push(value) {
        if (this.disposed)
            return;
        this.pending = value;
        if (this.timer === undefined) {
            this.timer = this.scheduler.set(() => this.commit(), this.intervalMs);
        }
    }
    /** 立即提交挂起值（例如卸载前或恢复可见时）。 */
    flushNow() {
        this.commit();
    }
    /** 丢弃挂起值并取消定时器。 */
    dispose() {
        this.disposed = true;
        this.pending = undefined;
        if (this.timer !== undefined) {
            this.scheduler.clear(this.timer);
            this.timer = undefined;
        }
    }
    commit() {
        if (this.timer !== undefined) {
            this.scheduler.clear(this.timer);
            this.timer = undefined;
        }
        if (this.disposed)
            return;
        const value = this.pending;
        this.pending = undefined;
        if (value !== undefined)
            this.flush(value);
    }
}
