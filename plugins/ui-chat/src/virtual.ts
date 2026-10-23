/**
 * 虚拟列表与增量渲染的纯逻辑工具。
 *
 * 这里不依赖 React，便于单元测试；组件层只负责把结果映射到 DOM。
 */

/** 虚拟窗口输入。 */
export interface VirtualWindowInput {
  /** 当前滚动偏移（px）。 */
  scrollTop: number;
  /** 可视区高度（px）。 */
  viewportHeight: number;
  /** 单条消息的估算高度（px）。 */
  itemHeight: number;
  /** 消息总数。 */
  count: number;
  /** 可视区上下各多渲染的条数，默认 5。 */
  overscan?: number;
}

/** 虚拟窗口输出。 */
export interface VirtualWindow {
  /** 起始索引（含）。 */
  start: number;
  /** 结束索引（不含）。 */
  end: number;
  /** 起始项在滚动容器中的偏移（px）。 */
  offsetY: number;
  /** 全部消息的估算总高度（px）。 */
  totalHeight: number;
}

/**
 * 计算只渲染可视区及上下缓冲的窗口。
 *
 * @param input 滚动、可视区与消息数量。
 * @returns 起止索引与占位高度；滚动越界时收敛到合法范围。
 */
export function computeVirtualWindow(input: VirtualWindowInput): VirtualWindow {
  const overscan = Math.max(0, Math.trunc(input.overscan ?? 5));
  const itemHeight = Math.max(1, input.itemHeight);
  const count = Math.max(0, Math.trunc(input.count));
  if (count === 0) return { start: 0, end: 0, offsetY: 0, totalHeight: 0 };

  const scrollTop = Math.max(0, input.scrollTop);
  const firstVisible = Math.floor(scrollTop / itemHeight);
  const visibleCount = Math.ceil(Math.max(0, input.viewportHeight) / itemHeight) + 1;
  // 滚动超出内容时把窗口收敛到末尾，避免 start > count 产生空渲染。
  const maxStart = Math.max(0, count - visibleCount - overscan);
  const start = Math.min(maxStart, Math.max(0, firstVisible - overscan));
  const end = Math.min(count, start + visibleCount + overscan);
  return {
    start,
    end,
    offsetY: start * itemHeight,
    totalHeight: count * itemHeight,
  };
}

/** 定时器抽象，测试可注入受控实现。 */
export interface Scheduler {
  /** 安排一次回调。 */
  set(callback: () => void, delayMs: number): unknown;
  /** 取消已安排的回调。 */
  clear(handle: unknown): void;
}

/** 默认调度器：浏览器/Node 的 setTimeout。 */
export const defaultScheduler: Scheduler = {
  set: (callback, delayMs) => setTimeout(callback, delayMs),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/**
 * 100ms 批量缓冲：流式 token 只更新“最新值”，每 100ms 最多提交一次。
 *
 * 与简单 debounce 不同，定时器一旦启动就不会被新值重置，因此持续流式输出
 * 时仍能保证稳定刷新频率。
 */
export class BatchBuffer<T> {
  private readonly intervalMs: number;
  private readonly flush: (value: T) => void;
  private readonly scheduler: Scheduler;
  private pending: T | undefined;
  private timer: unknown;
  private disposed = false;

  /**
   * @param flush 批次提交回调。
   * @param intervalMs 提交间隔；默认 100ms。
   * @param scheduler 定时器实现；默认 setTimeout。
   */
  constructor(
    flush: (value: T) => void,
    intervalMs = 100,
    scheduler: Scheduler = defaultScheduler,
  ) {
    this.flush = flush;
    this.intervalMs = intervalMs;
    this.scheduler = scheduler;
  }

  /** 写入最新值；存在未提交定时器时不重置它。 */
  push(value: T): void {
    if (this.disposed) return;
    this.pending = value;
    if (this.timer === undefined) {
      this.timer = this.scheduler.set(() => this.commit(), this.intervalMs);
    }
  }

  /** 立即提交挂起值（例如卸载前或恢复可见时）。 */
  flushNow(): void {
    this.commit();
  }

  /** 丢弃挂起值并取消定时器。 */
  dispose(): void {
    this.disposed = true;
    this.pending = undefined;
    if (this.timer !== undefined) {
      this.scheduler.clear(this.timer);
      this.timer = undefined;
    }
  }

  private commit(): void {
    if (this.timer !== undefined) {
      this.scheduler.clear(this.timer);
      this.timer = undefined;
    }
    if (this.disposed) return;
    const value = this.pending;
    this.pending = undefined;
    if (value !== undefined) this.flush(value);
  }
}
