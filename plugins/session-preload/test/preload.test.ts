import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import {
  cacheDirectory,
  listSessionCacheFiles,
  preloadFromDisk,
  preloadRecentSessions,
  readProjection,
  resolveDshHome,
  SessionPreloadCache,
} from '../src/preload.ts';

/** 在临时 DSH_HOME 中写入一个投影缓存。 */
async function writeCache(
  home: string,
  id: string,
  title: string,
  cwd = 'D:\\Dev',
): Promise<string> {
  const directory = cacheDirectory(home);
  await mkdir(directory, { recursive: true });
  const path = join(directory, `${id}.json`);
  await writeFile(
    path,
    JSON.stringify({
      version: 7,
      record: {
        identity: { cwd, createdAt: 1 },
        rows: { title: { ver: 1, seq: 2, val: title } },
      },
    }),
    'utf8',
  );
  return path;
}

test('resolveDshHome 优先 DSH_HOME，其次用户目录', () => {
  assert.equal(resolveDshHome({ DSH_HOME: 'D:\\isolated' } as NodeJS.ProcessEnv), 'D:\\isolated');
  assert.match(
    resolveDshHome({ USERPROFILE: 'C:\\Users\\tester' } as NodeJS.ProcessEnv),
    /\.dsh$/,
  );
});

test('readProjection 解析标题、cwd 与 rows，损坏文件返回 undefined', async () => {
  const home = await mkdtemp(join(tmpdir(), 'inkdsh-sp-'));
  try {
    const path = await writeCache(home, 's1', '标题一');
    const projection = await readProjection(path);
    assert.equal(projection?.title, '标题一');
    assert.equal(projection?.cwd, 'D:\\Dev');
    assert.ok(projection?.rows.title);

    const broken = join(cacheDirectory(home), 'broken.json');
    await writeFile(broken, '{not json', 'utf8');
    assert.equal(await readProjection(broken), undefined);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('listSessionCacheFiles 按 mtime 倒序并遵守 limit', async () => {
  const home = await mkdtemp(join(tmpdir(), 'inkdsh-sp-'));
  try {
    const first = await writeCache(home, 'old', '旧');
    const second = await writeCache(home, 'new', '新');
    await utimes(first, new Date(1_000), new Date(1_000));
    await utimes(second, new Date(2_000), new Date(2_000));

    const files = await listSessionCacheFiles(home, 1);
    assert.deepEqual(
      files.map((file) => file.id),
      ['new'],
    );
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('preloadFromDisk 缓存最近的会话投影', async () => {
  const home = await mkdtemp(join(tmpdir(), 'inkdsh-sp-'));
  try {
    await writeCache(home, 's1', '一');
    await writeCache(home, 's2', '二');
    const cache = new SessionPreloadCache();
    const stats = await preloadFromDisk(home, cache, 10);
    assert.equal(stats.count, 2);
    assert.equal(cache.size, 2);
    assert.equal(cache.get('s1')?.projection.asOfSeq, -1);
    assert.equal(cache.get('s2')?.live, false);

    cache.clear();
    assert.equal(cache.size, 0);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('preloadRecentSessions 对活会话使用投影快照', async () => {
  const home = await mkdtemp(join(tmpdir(), 'inkdsh-sp-'));
  try {
    await writeCache(home, 'live-1', '磁盘标题');
    const cache = new SessionPreloadCache();
    const sessions = { get: (id: string) => (id === 'live-1' ? { id } : undefined) };
    const projections = {
      snapshot: () => ({ asOfSeq: 42, values: { title: '活标题' } }),
    };

    const stats = await preloadRecentSessions(sessions, projections, home, cache, 10);
    assert.equal(stats.count, 1);
    assert.equal(stats.live, 1);
    const entry = cache.get('live-1');
    assert.equal(entry?.live, true);
    assert.equal(entry?.projection.asOfSeq, 42);
    assert.deepEqual(entry?.projection.rows, { title: '活标题' });
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('目录缺失时预折叠返回空统计而不是抛错', async () => {
  const home = await mkdtemp(join(tmpdir(), 'inkdsh-sp-'));
  try {
    const stats = await preloadFromDisk(home, new SessionPreloadCache(), 10);
    assert.equal(stats.count, 0);
    assert.equal(stats.live, 0);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});
