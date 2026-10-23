/**
 * 会话预折叠：启动时读取 `session_projcache/sessions/*.json`，把最近会话的
 * 投影状态缓存到内存，避免客户端首次列会话时逐个冷读日志。
 *
 * 本文件保持与 Cordis 无关，便于用临时目录做纯单元测试；插件入口在
 * `index.ts` 里完成注入与生命周期绑定。
 */
import { readFile, readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';

/** 一个会话在内存里的投影状态。 */
export interface ProjectionState {
  /** 投影行（`record.rows` 的浅层键值）；值为持久化 JSON。 */
  readonly rows: Readonly<Record<string, unknown>>;
  /** 投影反映到的最后一个事件序号；未知时为 -1。 */
  readonly asOfSeq: number;
}

/** 预折叠后的单会话缓存项。 */
export interface PreloadedSession {
  /** 会话 id（缓存文件名去掉 `.json`）。 */
  readonly id: string;
  /** 投影中的标题；缺失为空串。 */
  readonly title: string;
  /** 会话创建目录；缺失为空串。 */
  readonly cwd: string;
  /** 缓存文件修改时间（Unix 毫秒）。 */
  readonly updatedAt: number;
  /** 投影状态。 */
  readonly projection: ProjectionState;
  /** 是否来自活会话的投影快照（而非磁盘缓存）。 */
  readonly live: boolean;
}

/** 预折叠统计，供日志与测试断言。 */
export interface PreloadStats {
  /** 成功折叠的会话数。 */
  readonly count: number;
  /** 其中来自活会话快照的数量。 */
  readonly live: number;
  /** 扫描目录；目录不存在时仍返回路径。 */
  readonly directory: string;
}

/** 内存会话投影缓存。 */
export class SessionPreloadCache {
  private readonly sessions = new Map<string, PreloadedSession>();

  /** 写入或覆盖一个会话。 */
  set(entry: PreloadedSession): void {
    this.sessions.set(entry.id, entry);
  }

  /** 按 id 读取会话。 */
  get(id: string): PreloadedSession | undefined {
    return this.sessions.get(id);
  }

  /** 当前缓存数量。 */
  get size(): number {
    return this.sessions.size;
  }

  /** 按最近更新时间倒序返回全部条目。 */
  values(): PreloadedSession[] {
    return [...this.sessions.values()].sort((left, right) => right.updatedAt - left.updatedAt);
  }

  /** 清空缓存。 */
  clear(): void {
    this.sessions.clear();
  }
}

/** 会话投影缓存目录。 */
export function cacheDirectory(dshHome: string): string {
  return join(dshHome, 'storages', 'session_projcache', 'sessions');
}

/**
 * 解析 DSH_HOME。
 *
 * @param env 进程环境；测试可注入。
 * @returns DSH_HOME，或用户目录下的 `.dsh`。
 */
export function resolveDshHome(env: NodeJS.ProcessEnv = process.env): string {
  if (env.DSH_HOME) return env.DSH_HOME;
  const home = env.USERPROFILE ?? env.HOME;
  if (!home) throw new Error('无法解析 DSH_HOME：缺少 USERPROFILE/HOME');
  return join(home, '.dsh');
}

/**
 * 列出最近修改的会话缓存文件。
 *
 * @param home DSH_HOME。
 * @param limit 最多返回的文件数。
 * @returns 按 mtime 倒序的文件描述；目录不存在时为空数组。
 */
export async function listSessionCacheFiles(
  home: string,
  limit = 10,
): Promise<Array<{ id: string; path: string; updatedAt: number }>> {
  const directory = cacheDirectory(home);
  let names: string[];
  try {
    names = await readdir(directory);
  } catch {
    return [];
  }

  const files: Array<{ id: string; path: string; updatedAt: number }> = [];
  for (const name of names) {
    if (!name.endsWith('.json')) continue;
    const path = join(directory, name);
    try {
      const info = await stat(path);
      if (!info.isFile()) continue;
      files.push({
        id: name.slice(0, -'.json'.length),
        path,
        updatedAt: Math.trunc(info.mtimeMs),
      });
    } catch {
      // 文件在扫描与 stat 之间消失：跳过，不影响其它会话。
    }
  }

  files.sort((left, right) => right.updatedAt - left.updatedAt);
  return files.slice(0, limit);
}

/**
 * 读取一个投影缓存文件。
 *
 * @param path 缓存文件路径。
 * @returns 解析出的行与版本；损坏或非对象时返回 undefined。
 */
export async function readProjection(
  path: string,
): Promise<{ rows: Readonly<Record<string, unknown>>; title: string; cwd: string } | undefined> {
  try {
    const text = await readFile(path, 'utf8');
    const value: unknown = JSON.parse(text);
    if (typeof value !== 'object' || value === null) return undefined;
    const record = (value as { record?: unknown }).record;
    if (typeof record !== 'object' || record === null) return undefined;
    const rows = (record as { rows?: unknown }).rows;
    if (typeof rows !== 'object' || rows === null) return undefined;
    const identity = (record as { identity?: unknown }).identity;
    const identityRecord =
      typeof identity === 'object' && identity !== null
        ? (identity as { cwd?: unknown })
        : undefined;
    const titleRow = (rows as { title?: unknown }).title;
    const titleValue =
      typeof titleRow === 'object' && titleRow !== null
        ? (titleRow as { val?: unknown }).val
        : undefined;
    return {
      rows: rows as Record<string, unknown>,
      title: typeof titleValue === 'string' ? titleValue : '',
      cwd: typeof identityRecord?.cwd === 'string' ? identityRecord.cwd : '',
    };
  } catch {
    return undefined;
  }
}

/**
 * 从磁盘缓存预折叠最近会话。
 *
 * @param home DSH_HOME。
 * @param cache 目标内存缓存。
 * @param limit 最多折叠数量。
 * @returns 统计信息；目录缺失时 count 为 0。
 */
export async function preloadFromDisk(
  home: string,
  cache: SessionPreloadCache,
  limit = 10,
): Promise<PreloadStats> {
  const directory = cacheDirectory(home);
  const files = await listSessionCacheFiles(home, limit);
  let count = 0;
  for (const file of files) {
    const projection = await readProjection(file.path);
    if (!projection) continue;
    cache.set({
      id: file.id,
      title: projection.title,
      cwd: projection.cwd,
      updatedAt: file.updatedAt,
      projection: { rows: projection.rows, asOfSeq: -1 },
      live: false,
    });
    count += 1;
  }
  return { count, live: 0, directory };
}

/**
 * 预折叠最近会话，优先使用活会话的投影快照。
 *
 * @param sessions 只读的会话服务句柄。
 * @param projections 只读的投影注册表句柄。
 * @param home DSH_HOME。
 * @param cache 目标缓存。
 * @param limit 最多处理数量。
 * @returns 统计信息。
 */
export async function preloadRecentSessions(
  sessions: { get(id: string): unknown },
  projections: { snapshot(session: never): { asOfSeq: number; values: Record<string, unknown> } },
  home: string,
  cache: SessionPreloadCache,
  limit = 10,
): Promise<PreloadStats> {
  const directory = cacheDirectory(home);
  const files = await listSessionCacheFiles(home, limit);
  let count = 0;
  let live = 0;

  for (const file of files) {
    const disk = await readProjection(file.path);
    if (!disk) continue;

    let projection: ProjectionState = { rows: disk.rows, asOfSeq: -1 };
    let isLive = false;
    const session = sessions.get(file.id);
    if (session !== undefined) {
      try {
        const snapshot = projections.snapshot(session as never);
        projection = { rows: snapshot.values as Record<string, unknown>, asOfSeq: snapshot.asOfSeq };
        isLive = true;
        live += 1;
      } catch {
        // 活会话快照不可用时退回磁盘行，不影响其它会话。
      }
    }

    cache.set({
      id: file.id,
      title: disk.title,
      cwd: disk.cwd,
      updatedAt: file.updatedAt,
      projection,
      live: isLive,
    });
    count += 1;
  }

  return { count, live, directory };
}
