/**
 * 会话预折叠：启动时读取 `session_projcache/sessions/*.json`，把最近会话的
 * 投影状态缓存到内存，避免客户端首次列会话时逐个冷读日志。
 *
 * 本文件保持与 Cordis 无关，便于用临时目录做纯单元测试；插件入口在
 * `index.ts` 里完成注入与生命周期绑定。
 */
import { readFile, readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';
/** 内存会话投影缓存。 */
export class SessionPreloadCache {
    sessions = new Map();
    /** 写入或覆盖一个会话。 */
    set(entry) {
        this.sessions.set(entry.id, entry);
    }
    /** 按 id 读取会话。 */
    get(id) {
        return this.sessions.get(id);
    }
    /** 当前缓存数量。 */
    get size() {
        return this.sessions.size;
    }
    /** 按最近更新时间倒序返回全部条目。 */
    values() {
        return [...this.sessions.values()].sort((left, right) => right.updatedAt - left.updatedAt);
    }
    /** 清空缓存。 */
    clear() {
        this.sessions.clear();
    }
}
/** 会话投影缓存目录。 */
export function cacheDirectory(dshHome) {
    return join(dshHome, 'storages', 'session_projcache', 'sessions');
}
/**
 * 解析 DSH_HOME。
 *
 * @param env 进程环境；测试可注入。
 * @returns DSH_HOME，或用户目录下的 `.dsh`。
 */
export function resolveDshHome(env = process.env) {
    if (env.DSH_HOME)
        return env.DSH_HOME;
    const home = env.USERPROFILE ?? env.HOME;
    if (!home)
        throw new Error('无法解析 DSH_HOME：缺少 USERPROFILE/HOME');
    return join(home, '.dsh');
}
/**
 * 列出最近修改的会话缓存文件。
 *
 * @param home DSH_HOME。
 * @param limit 最多返回的文件数。
 * @returns 按 mtime 倒序的文件描述；目录不存在时为空数组。
 */
export async function listSessionCacheFiles(home, limit = 10) {
    const directory = cacheDirectory(home);
    let names;
    try {
        names = await readdir(directory);
    }
    catch {
        return [];
    }
    const files = [];
    for (const name of names) {
        if (!name.endsWith('.json'))
            continue;
        const path = join(directory, name);
        try {
            const info = await stat(path);
            if (!info.isFile())
                continue;
            files.push({
                id: name.slice(0, -'.json'.length),
                path,
                updatedAt: Math.trunc(info.mtimeMs),
            });
        }
        catch {
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
export async function readProjection(path) {
    try {
        const text = await readFile(path, 'utf8');
        const value = JSON.parse(text);
        if (typeof value !== 'object' || value === null)
            return undefined;
        const record = value.record;
        if (typeof record !== 'object' || record === null)
            return undefined;
        const rows = record.rows;
        if (typeof rows !== 'object' || rows === null)
            return undefined;
        const identity = record.identity;
        const identityRecord = typeof identity === 'object' && identity !== null
            ? identity
            : undefined;
        const titleRow = rows.title;
        const titleValue = typeof titleRow === 'object' && titleRow !== null
            ? titleRow.val
            : undefined;
        return {
            rows: rows,
            title: typeof titleValue === 'string' ? titleValue : '',
            cwd: typeof identityRecord?.cwd === 'string' ? identityRecord.cwd : '',
        };
    }
    catch {
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
export async function preloadFromDisk(home, cache, limit = 10) {
    const directory = cacheDirectory(home);
    const files = await listSessionCacheFiles(home, limit);
    let count = 0;
    for (const file of files) {
        const projection = await readProjection(file.path);
        if (!projection)
            continue;
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
export async function preloadRecentSessions(sessions, projections, home, cache, limit = 10) {
    const directory = cacheDirectory(home);
    const files = await listSessionCacheFiles(home, limit);
    let count = 0;
    let live = 0;
    for (const file of files) {
        const disk = await readProjection(file.path);
        if (!disk)
            continue;
        let projection = { rows: disk.rows, asOfSeq: -1 };
        let isLive = false;
        const session = sessions.get(file.id);
        if (session !== undefined) {
            try {
                const snapshot = projections.snapshot(session);
                projection = { rows: snapshot.values, asOfSeq: snapshot.asOfSeq };
                isLive = true;
                live += 1;
            }
            catch {
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
