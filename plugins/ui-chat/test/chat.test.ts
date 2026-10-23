import assert from 'node:assert/strict';
import { test } from 'node:test';

import { nodeKey, nodeLabel, nodeText, type ChatNodeLike } from '../src/nodes.ts';
import { register } from '../src/register.ts';
import { computeVirtualWindow, BatchBuffer, type Scheduler } from '../src/virtual.ts';

test('computeVirtualWindow 只渲染可视区并保留上下缓冲', () => {
  const window = computeVirtualWindow({
    scrollTop: 1_000,
    viewportHeight: 500,
    itemHeight: 100,
    count: 100,
    overscan: 5,
  });
  assert.equal(window.start, 5);
  assert.equal(window.end, 16);
  assert.equal(window.offsetY, 500);
  assert.equal(window.totalHeight, 10_000);
});

test('computeVirtualWindow 在首尾与空列表收敛', () => {
  assert.deepEqual(
    computeVirtualWindow({ scrollTop: 0, viewportHeight: 500, itemHeight: 100, count: 3 }),
    { start: 0, end: 3, offsetY: 0, totalHeight: 300 },
  );
  assert.deepEqual(
    computeVirtualWindow({ scrollTop: 99_999, viewportHeight: 500, itemHeight: 100, count: 20 }),
    { start: 9, end: 20, offsetY: 900, totalHeight: 2_000 },
  );
  assert.deepEqual(
    computeVirtualWindow({ scrollTop: 10, viewportHeight: 10, itemHeight: 100, count: 0 }),
    { start: 0, end: 0, offsetY: 0, totalHeight: 0 },
  );
});

test('BatchBuffer 每 100ms 只提交一次最新值，且不被新值重置', () => {
  let now = 0;
  let timer: (() => void) | undefined;
  const scheduler: Scheduler = {
    set(callback) {
      timer = callback;
      now += 100;
      return 1;
    },
    clear() {
      timer = undefined;
    },
  };
  const flushed: number[][] = [];
  const buffer = new BatchBuffer<number[]>((value) => flushed.push(value), 100, scheduler);

  buffer.push([1]);
  buffer.push([1, 2]);
  buffer.push([1, 2, 3]);
  assert.deepEqual(flushed, []);
  timer?.();
  assert.deepEqual(flushed, [[1, 2, 3]]);

  buffer.push([4]);
  buffer.flushNow();
  assert.deepEqual(flushed, [[1, 2, 3], [4]]);
  buffer.dispose();
  buffer.push([5]);
  assert.deepEqual(flushed, [[1, 2, 3], [4]]);
});

test('nodeText/nodeLabel 兼容用户、助手、工具与未知节点', () => {
  const user: ChatNodeLike = { kind: 'message.user', data: { text: '你好' } };
  const assistant: ChatNodeLike = {
    kind: 'message.assistant',
    data: { blocks: [{ text: '第一段' }, { text: '第二段' }] },
  };
  const tool: ChatNodeLike = { kind: 'tool.call', data: { name: 'bash', input: { cmd: 'ls' } } };
  assert.equal(nodeText(user), '你好');
  assert.equal(nodeText(assistant), '第一段\n第二段');
  assert.match(nodeText(tool), /bash/);
  assert.equal(nodeLabel(user), '你');
  assert.equal(nodeLabel(assistant), 'DSH');
  assert.equal(nodeLabel(tool), '工具');
  assert.equal(nodeKey({ kind: 'x', id: 'a' }, 3), 'x:a');
});

test('register 在 slots 可用时注册独立视图，不可用时降级', () => {
  const registrations: Array<{ name: string; id: unknown; component: unknown }> = [];
  let injected: string | undefined;
  const ctx = {
    slots: {
      inject(name: string, factory: () => unknown) {
        injected = name;
        factory();
      },
      register(entry: Record<string, unknown>, component: unknown) {
        registrations.push({ name: String(entry.name), id: entry.id, component });
        return () => undefined;
      },
    },
    sessions: {
      binding: () => ({ target: () => ({ getSnapshot: () => ({ nodes: [] }) }) }),
    },
  };
  assert.equal(register(ctx as never), true);
  assert.equal(injected, 'conversation.view');
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0]?.id, 'inkdsh-chat');
  assert.equal(typeof registrations[0]?.component, 'function');

  assert.equal(register({} as never), false);
});
