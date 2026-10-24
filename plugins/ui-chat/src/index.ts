/**
 * `@inkdsh/ui-chat` host 入口。
 *
 * 复用官方 host 半边的设置声明（transcriptView / collapseTiming /
 * performanceUsage / linkOpening），使客户端设置表单与官方行为一致。
 */
export { apply, Config } from './official/host.js';
