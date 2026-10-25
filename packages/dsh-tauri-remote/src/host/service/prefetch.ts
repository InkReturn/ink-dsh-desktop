import { Buffer } from 'node:buffer'
import { get } from 'node:http'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'
import { gateway } from './gateway'
import { cachedAssetExists, cachePathOf } from './static-cache'

/**
 * P3c：连接成功后静默拉取首页引用的静态资源，交给 gateway 的 tee 写入磁盘缓存。
 *
 * - 只处理首页 HTML 中直接引用的同源资源（核心 assets、插件合并 bundle、icon、manifest）；
 * - 已有完整缓存的文件跳过，避免每次重连重复下载；
 * - 每个请求都带 `x-inkdsh-prefetch: 1`：gateway 不把预加载算作用户活动；
 * - 每次请求前等待用户请求空闲（默认 2s），用户正在流式输出时自动暂停；
 * - 串行拉取，避免和前台页面抢带宽。
 */

const INITIAL_DELAY_MS = 5_000
const IDLE_MS = 2_000
const REQUEST_TIMEOUT_MS = 30_000
const MAX_HTML_BYTES = 4 * 1024 * 1024
const MAX_URLS = 256

interface PrefetchOptions {
  machineId: string
  gatewayUrl: string
  identity: string
  /** 测试可覆盖；默认读 INKDSH_REMOTE_CACHE_DIR。 */
  cacheRoot?: string
  /** 测试可设为 0；默认连接后等 5s，避开首屏。 */
  initialDelayMs?: number
  /** 测试可设为 0；默认 2s 无用户请求才开始下一个文件。 */
  idleMs?: number
}

interface RunningPrefetch {
  controller: AbortController
}

const running = new Map<string, RunningPrefetch>()

export function cancel(machineId: string): void {
  const current = running.get(machineId)
  if (current === undefined)
    return
  running.delete(machineId)
  current.controller.abort()
}

/** 连接成功后调用；重复调用会取消上一轮。 */
export function start(options: PrefetchOptions): void {
  cancel(options.machineId)
  const controller = new AbortController()
  running.set(options.machineId, { controller })
  void runPrefetch(options, controller.signal)
    .catch((error: unknown) => {
      if (!isAbort(error))
        console.warn('[remote] prefetch failed:', String(error))
    })
    .finally(() => {
      if (running.get(options.machineId)?.controller === controller)
        running.delete(options.machineId)
    })
}

/** 测试/集成用：等待一轮预加载结束。 */
export async function runNow(options: PrefetchOptions): Promise<{ fetched: number, skipped: number }> {
  return await runPrefetch(options, new AbortController().signal)
}

async function runPrefetch(options: PrefetchOptions, signal: AbortSignal): Promise<{ fetched: number, skipped: number }> {
  const cacheRoot = options.cacheRoot ?? process.env.INKDSH_REMOTE_CACHE_DIR
  if (cacheRoot === undefined || cacheRoot === '')
    return { fetched: 0, skipped: 0 }
  if (options.initialDelayMs !== 0)
    await sleep(options.initialDelayMs ?? INITIAL_DELAY_MS, undefined, { signal })
  const gatewayUrl = new URL(options.gatewayUrl)
  const html = await getText(gatewayUrl.href, signal)
  const urls = extractPrefetchUrls(html, gatewayUrl.href)
  let fetched = 0
  let skipped = 0
  for (const url of urls) {
    const key = cachePathOf(url)
    if (key === undefined)
      continue
    if (await cachedAssetExists(cacheRoot, options.machineId, options.identity, key)) {
      skipped += 1
      continue
    }
    await waitForIdle(options.machineId, options.idleMs ?? IDLE_MS, signal)
    await drain(url, signal)
    fetched += 1
    await sleep(100, undefined, { signal })
  }
  return { fetched, skipped }
}

/** 从首页 HTML 提取同源静态资源；只保留缓存策略认可、可直接预取的 URL。 */
export function extractPrefetchUrls(html: string, baseUrl: string): string[] {
  const baseHref = /<base\b[^>]+?href\s*=\s*["']([^"']+)["']/iu.exec(html)?.[1]
  const base = new URL(baseHref ?? '/', baseUrl)
  const origin = new URL(baseUrl).origin
  const urls: string[] = []
  const seen = new Set<string>()
  const markup = html.replace(/<!--[\s\S]*?-->/gu, '')
  for (const match of markup.matchAll(/<(?:script|link)\b[^>]+?(?:src|href)\s*=\s*["']([^"']+)["'][^>]*>/giu)) {
    const reference = match[1].replaceAll('&amp;', '&').trim()
    if (reference === '')
      continue
    let url: URL
    try {
      url = new URL(reference, base)
    }
    catch {
      continue
    }
    if (url.origin !== origin || cachePathOf(url.href) === undefined || seen.has(url.href))
      continue
    seen.add(url.href)
    urls.push(url.href)
    if (urls.length >= MAX_URLS)
      break
  }
  return urls
}

async function waitForIdle(machineId: string, idleMs: number, signal: AbortSignal): Promise<void> {
  for (;;) {
    if (signal.aborted)
      throw signal.reason ?? new Error('prefetch aborted')
    const activity = gateway.activity(`remote-outbound:${machineId}`)
    if (activity === undefined)
      throw new Error('prefetch gateway is no longer running')
    if (activity.active === 0 && activity.idleMs >= idleMs)
      return
    await sleep(250, undefined, { signal })
  }
}

function getText(url: string, signal: AbortSignal): Promise<string> {
  return new Promise((resolve, reject) => {
    const request = get(url, { headers: prefetchHeaders('text/html') }, (response) => {
      const status = response.statusCode ?? 0
      if (status < 200 || status >= 300) {
        response.resume()
        reject(new Error(`prefetch html status ${status}`))
        return
      }
      const chunks: Buffer[] = []
      let bytes = 0
      response.on('data', (chunk: Buffer) => {
        bytes += chunk.length
        if (bytes > MAX_HTML_BYTES) {
          request.destroy(new Error('prefetch html too large'))
          return
        }
        chunks.push(chunk)
      })
      response.on('end', () => resolve(Buffer.concat(chunks, bytes).toString('utf8')))
      response.on('error', reject)
    })
    request.setTimeout(REQUEST_TIMEOUT_MS, () => request.destroy(new Error('prefetch html timeout')))
    request.on('error', reject)
    signal.addEventListener('abort', () => request.destroy(new Error('prefetch aborted')), { once: true })
  })
}

function drain(url: string, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = get(url, { headers: prefetchHeaders('*/*') }, (response) => {
      const status = response.statusCode ?? 0
      if (status < 200 || status >= 300) {
        response.resume()
        reject(new Error(`prefetch asset status ${status}`))
        return
      }
      response.on('end', resolve)
      response.on('error', reject)
      response.resume()
    })
    request.setTimeout(REQUEST_TIMEOUT_MS, () => request.destroy(new Error('prefetch asset timeout')))
    request.on('error', reject)
    signal.addEventListener('abort', () => request.destroy(new Error('prefetch aborted')), { once: true })
  })
}

function prefetchHeaders(accept: string): Record<string, string> {
  return {
    'accept': accept,
    'accept-encoding': 'identity',
    'x-inkdsh-prefetch': '1',
  }
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && (error.name === 'AbortError' || error.message === 'prefetch aborted')
}
