import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { computeVirtualWindow, BatchBuffer, type Scheduler } from '../src/virtual.ts';
import { chatRenderKey } from '../src/official/client/chat/render-entry.ts';
import { EMPTY_CHAT_SNAPSHOT } from '../src/official/client/contract/snapshot.ts';

const packageDir = join(dirname(fileURLToPath(import.meta.url)), '..');
const officialDir = join(packageDir, 'src/official');

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
  let timer: (() => void) | undefined;
  const scheduler: Scheduler = {
    set(callback) {
      timer = callback;
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

test('官方 chatRenderKey 与空快照保持可用', () => {
  assert.equal(
    chatRenderKey({ kind: 'node', key: 'n1' } as never),
    JSON.stringify(['node', 'n1', null]),
  );
  assert.equal(
    chatRenderKey({ kind: 'group', key: 'g1' } as never),
    JSON.stringify(['group', 'g1']),
  );
  assert.deepEqual(EMPTY_CHAT_SNAPSHOT.order, []);
});

test('官方 ui-chat 源码目录结构完整', () => {
  const required = [
    'client/apply.ts',
    'client/chat/ChatView.tsx',
    'client/chat/MessageItem.tsx',
    'client/chat/use-chat-viewport.ts',
    'client/contract/slots.ts',
    'client/conversation-nodes/chat-snapshot-builder.ts',
    'client/model/tool-call-tree.ts',
    'client/settings/TranscriptViewRow.tsx',
    'chat-settings.ts',
  ];
  for (const relative of required) {
    assert.ok(existsSync(join(officialDir, relative)), `missing ${relative}`);
  }
  assert.ok(existsSync(join(officialDir, 'client/chat/ChatView.module.css')));
  assert.ok(existsSync(join(officialDir, 'client/chat/running-whale@2x.png')));
});

test('client bundle 使用 __ModuleLoader__ 且只外置 loader 模块表依赖', () => {
  const bundlePath = join(packageDir, 'lib/client.js');
  // 先跑 build 再跑 test；缺失时明确失败而不是假绿。
  if (!existsSync(bundlePath)) {
    assert.fail('lib/client.js 不存在，请先运行 pnpm --filter @inkdsh/ui-chat build');
  }
  const bundle = readFileSync(bundlePath, 'utf8');
  assert.match(bundle, /window\.__ModuleLoader__\.load\(\{/);
  assert.match(bundle, /id: "@inkdsh\/ui-chat"/);
  assert.match(bundle, /factory: \(require\) =>/);
  const externals = [...bundle.matchAll(/require\("([^"]+)"\)/g)].map((match) => match[1]);
  assert.deepEqual(
    [...new Set(externals)].sort(),
    [
      '@deepseek-ai/dsh-client-store',
      '@deepseek-ai/dsh-client-ui-primitives',
      '@tanstack/react-virtual',
      'react',
      'react-dom',
      'react/jsx-runtime',
    ].sort(),
  );
  assert.match(bundle, /data-plugin-css/);
});
