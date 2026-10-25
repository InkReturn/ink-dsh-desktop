import type { AddressInfo } from 'node:net'
import { mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { setSourceSession } from '../config/runtime'
import { gateway } from './gateway'
import { runNow } from './prefetch'
import { cachedAssetExists } from './static-cache'

setSourceSession('prefetch-integration')
const machineId = 'prefetch-test'
const entryId = `remote-outbound:${machineId}`
let cacheRoot: string | undefined
let previousCache: string | undefined

afterEach(async () => {
  await gateway.stop(entryId).catch(() => undefined)
  await gateway.dispose()
  if (previousCache === undefined)
    delete process.env.INKDSH_REMOTE_CACHE_DIR
  else
    process.env.INKDSH_REMOTE_CACHE_DIR = previousCache
  previousCache = undefined
  if (cacheRoot !== undefined)
    await rm(cacheRoot, { recursive: true, force: true })
  cacheRoot = undefined
})

describe('remote prefetch integration', () => {
  it('warms assets and versioned plugin bundles, then skips them on the next run', async () => {
    cacheRoot = await mkdtemp(join(tmpdir(), 'dsh-prefetch-cache-'))
    previousCache = process.env.INKDSH_REMOTE_CACHE_DIR
    process.env.INKDSH_REMOTE_CACHE_DIR = cacheRoot
    const requests: string[] = []
    const upstream = createServer((request, response) => {
      requests.push(request.url ?? '/')
      if (request.url === '/') {
        response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' })
        response.end(`<!doctype html><base href="./">
          <script src="/assets/index.js" type="module"></script>
          <link rel="stylesheet" href="/assets/index.css">
          <script src="/plugins/??@deepseek-ai/a/client.js,@local/b/client.js&amp;rev=123"></script>`)
        return
      }
      if (request.url?.startsWith('/assets/')) {
        response.writeHead(200, { 'content-type': request.url.endsWith('.css') ? 'text/css' : 'text/javascript' })
        response.end(`body:${request.url}`)
        return
      }
      if (request.url?.startsWith('/plugins/')) {
        response.writeHead(200, { 'content-type': 'text/javascript' })
        response.end('export const plugins = true')
        return
      }
      response.writeHead(404)
      response.end()
    })
    await new Promise<void>(resolve => upstream.listen(0, '127.0.0.1', resolve))
    try {
      const origin = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`
      const entry = await gateway.start({
        id: entryId,
        kind: 'outbound',
        upstream: origin,
        cacheIdentity: origin,
        port: 0,
        tokenProvider: async () => undefined,
      })
      const first = await runNow({
        machineId,
        gatewayUrl: `http://127.0.0.1:${entry.port}/`,
        identity: origin,
        cacheRoot,
        initialDelayMs: 0,
        idleMs: 0,
      })
      expect(first).toEqual({ fetched: 3, skipped: 0 })
      expect(requests).toContain('/plugins/??@deepseek-ai/a/client.js,@local/b/client.js&rev=123')
      await expect(cachedAssetExists(cacheRoot, machineId, origin, '/assets/index.js')).resolves.toBe(true)
      await expect(cachedAssetExists(cacheRoot, machineId, origin, '/assets/index.css')).resolves.toBe(true)
      await expect(cachedAssetExists(
        cacheRoot,
        machineId,
        origin,
        '/plugins/??@deepseek-ai/a/client.js,@local/b/client.js&rev=123',
      )).resolves.toBe(true)

      const second = await runNow({
        machineId,
        gatewayUrl: `http://127.0.0.1:${entry.port}/`,
        identity: origin,
        cacheRoot,
        initialDelayMs: 0,
        idleMs: 0,
      })
      expect(second).toEqual({ fetched: 0, skipped: 3 })
    }
    finally {
      await new Promise<void>((resolve, reject) => upstream.close(error => error === undefined ? resolve() : reject(error)))
    }
  })

  it('pauses asset prefetch while a user request is streaming', async () => {
    cacheRoot = await mkdtemp(join(tmpdir(), 'dsh-prefetch-cache-'))
    previousCache = process.env.INKDSH_REMOTE_CACHE_DIR
    process.env.INKDSH_REMOTE_CACHE_DIR = cacheRoot
    const requests: string[] = []
    const upstream = createServer((request, response) => {
      requests.push(request.url ?? '/')
      if (request.url === '/slow') {
        setTimeout(() => {
          response.writeHead(200, { 'content-type': 'text/plain' })
          response.end('stream')
        }, 350)
        return
      }
      if (request.url === '/') {
        response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' })
        response.end('<!doctype html><script src="/assets/late.js"></script>')
        return
      }
      if (request.url === '/assets/late.js') {
        response.writeHead(200, { 'content-type': 'text/javascript' })
        response.end('export const late = true')
        return
      }
      response.writeHead(404)
      response.end()
    })
    await new Promise<void>(resolve => upstream.listen(0, '127.0.0.1', resolve))
    try {
      const origin = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`
      const entry = await gateway.start({
        id: entryId,
        kind: 'outbound',
        upstream: origin,
        cacheIdentity: origin,
        port: 0,
        tokenProvider: async () => undefined,
      })
      const user = fetch(`http://127.0.0.1:${entry.port}/slow`)
      await new Promise(resolve => setTimeout(resolve, 30))
      const prefetching = runNow({
        machineId,
        gatewayUrl: `http://127.0.0.1:${entry.port}/`,
        identity: origin,
        cacheRoot,
        initialDelayMs: 0,
        idleMs: 0,
      })
      await new Promise(resolve => setTimeout(resolve, 100))
      expect(gateway.activity(entryId)?.active).toBe(1)
      expect(requests).not.toContain('/assets/late.js')
      await (await user).text()
      await expect(prefetching).resolves.toEqual({ fetched: 1, skipped: 0 })
      expect(requests).toContain('/assets/late.js')
    }
    finally {
      await new Promise<void>((resolve, reject) => upstream.close(error => error === undefined ? resolve() : reject(error)))
    }
  })
})
