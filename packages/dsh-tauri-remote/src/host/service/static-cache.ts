import type { IncomingMessage, OutgoingHttpHeaders, ServerResponse } from 'node:http'
import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { mkdir, readdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import process from 'node:process'

const MAX_BYTES = 64 * 1024 * 1024
const GLOBAL_LIMIT_BYTES = 500 * 1024 * 1024
const HOST_LIMIT_BYTES = 200 * 1024 * 1024

interface CacheFile {
  path: string
  hostId: string
  bytes: number
  modified: number
}

export function cachePathOf(url: string): string | undefined {
  try {
    const parsed = new URL(url, 'http://remote.invalid')
    const path = parsed.pathname
    if (parsed.search === '' && (/^\/assets\/[A-Za-z0-9][\w.-]*$/u.test(path) || ['/manifest.webmanifest', '/favicon.svg', '/favicon-dark.svg'].includes(path)))
      return path
    // DSH 的插件客户端 bundle 走 /plugins/??module,module&rev=hash 合并请求；
    // query 自带版本号，随插件更新自动换 key。
    if (path === '/plugins/' && parsed.search.startsWith('??') && parsed.search.length > 2 && parsed.search.length <= 8 * 1024)
      return `${path}${parsed.search}`
  }
  catch {
    // 非法 URL 一律不缓存。
  }
  return undefined
}

function cacheFile(root: string, hostId: string, identitySource: string, key: string): string | undefined {
  if (!/^[a-z0-9][a-z0-9-]*$/u.test(hostId) || identitySource === '')
    return undefined
  const identity = createHash('sha256').update(identitySource).digest('hex').slice(0, 16)
  return join(root, hostId, identity, createHash('sha256').update(key).digest('hex'))
}

/** 预加载去重用：缓存文件与 meta 都完整存在才算已缓存。 */
export async function cachedAssetExists(root: string | undefined, hostId: string, identitySource: string, key: string): Promise<boolean> {
  if (root === undefined)
    return false
  const file = cacheFile(root, hostId, identitySource, key)
  if (file === undefined)
    return false
  try {
    const metadata = JSON.parse(await readFile(`${file}.meta`, 'utf8')) as { path?: string }
    return metadata.path === key && (await stat(file)).isFile()
  }
  catch {
    return false
  }
}

/** HTML、401、Set-Cookie、私有/可协商响应都不能进入静态缓存。 */
export function isCacheableResponse(status: number | undefined, headers: IncomingMessage['headers']): boolean {
  if (status !== 200 || headers['set-cookie'] !== undefined || headers['content-location'] !== undefined)
    return false
  const cacheControl = String(headers['cache-control'] ?? '').toLowerCase()
  const vary = String(headers.vary ?? '').toLowerCase()
  return !/(?:^|,)\s*(?:no-store|private|no-cache)\s*(?:,|$)/u.test(cacheControl)
    && !/(?:^|,)\s*(?:cookie|authorization)\s*(?:,|$)/u.test(vary)
}

/** tee 已鉴权的响应体。任何写盘失败只丢弃本次缓存，绝不阻塞或改写用户响应。 */
export function cacheBody(root: string | undefined, hostId: string, identitySource: string, path: string, upstream: IncomingMessage, response: ServerResponse, headers: OutgoingHttpHeaders): void {
  if (root === undefined || !/^[a-z0-9][a-z0-9-]*$/u.test(hostId) || identitySource === '')
    return
  const cacheRoot: string = root
  const key = createHash('sha256').update(path).digest('hex')
  const directory = join(root, hostId)
  const identity = createHash('sha256').update(identitySource).digest('hex').slice(0, 16)
  const scopedDirectory = join(directory, identity)
  const temporary = join(scopedDirectory, `.${key}.${process.pid}.${Math.random().toString(16).slice(2)}.tmp`)
  const final = join(scopedDirectory, key)
  const chunks: Buffer[] = []
  let bytes = 0
  let complete = false
  let failed = false
  const onData = (chunk: Buffer): void => {
    bytes += chunk.length
    if (bytes > MAX_BYTES) {
      failed = true
      chunks.length = 0
      upstream.removeListener('data', onData)
      return
    }
    if (!failed)
      chunks.push(chunk)
  }
  const onEnd = (): void => {
    complete = true
    upstream.removeListener('data', onData)
    void persist()
  }
  const onError = (): void => {
    failed = true
    chunks.length = 0
  }
  const onClose = (): void => {
    if (!complete) {
      failed = true
      chunks.length = 0
      upstream.removeListener('data', onData)
    }
  }
  async function persist(): Promise<void> {
    if (failed || bytes > MAX_BYTES)
      return
    try {
      await mkdir(scopedDirectory, { recursive: true })
      await writeFile(temporary, Buffer.concat(chunks, bytes), { flag: 'wx' })
      await writeFile(`${temporary}.meta`, `${JSON.stringify({ path, fetchedAt: Date.now(), contentType: String(headers['content-type'] ?? 'application/octet-stream'), bytes })}\n`, 'utf8')
      await rename(temporary, final)
      await rename(`${temporary}.meta`, `${final}.meta`)
      await evict(cacheRoot, hostId)
    }
    catch {
      // 写盘或 LRU 失败只放弃本次缓存；客户端响应早已独立完成。
    }
    finally {
      void Promise.all([rm(temporary, { force: true }), rm(`${temporary}.meta`, { force: true })]).catch(() => undefined)
    }
  }
  upstream.on('data', onData)
  upstream.once('end', onEnd)
  upstream.once('error', onError)
  upstream.once('aborted', onError)
  response.once('close', onClose)
}

async function evict(root: string, currentHost: string): Promise<void> {
  const files: CacheFile[] = []
  for (const entry of await readdir(root, { withFileTypes: true }).catch(() => [])) {
    if (!entry.isDirectory() || !/^[a-z0-9][a-z0-9-]*$/u.test(entry.name))
      continue
    for (const scope of await readdir(join(root, entry.name), { withFileTypes: true })) {
      if (!scope.isDirectory())
        continue
      for (const file of await readdir(join(root, entry.name, scope.name))) {
        if (!/^[a-f0-9]{64}$/u.test(file))
          continue
        const path = join(root, entry.name, scope.name, file)
        const info = await stat(path).catch(() => undefined)
        if (info?.isFile())
          files.push({ path, hostId: entry.name, bytes: info.size, modified: info.mtimeMs })
      }
    }
  }
  let total = files.reduce((sum, file) => sum + file.bytes, 0)
  let hostTotal = files.filter(file => file.hostId === currentHost).reduce((sum, file) => sum + file.bytes, 0)
  files.sort((left, right) => Number(left.hostId === currentHost) - Number(right.hostId === currentHost) || left.modified - right.modified)
  for (const file of files) {
    if (total <= GLOBAL_LIMIT_BYTES && hostTotal <= HOST_LIMIT_BYTES)
      break
    await Promise.all([rm(file.path, { force: true }), rm(`${file.path}.meta`, { force: true })]).catch(() => undefined)
    total -= file.bytes
    if (file.hostId === currentHost)
      hostTotal -= file.bytes
  }
}
