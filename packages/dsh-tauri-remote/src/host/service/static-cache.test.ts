import type { IncomingMessage, ServerResponse } from 'node:http'
import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { afterEach, describe, expect, it } from 'vitest'
import { cacheBody, cachedAssetExists, cachePathOf, isCacheableResponse } from './static-cache'

const tempRoots: string[] = []
afterEach(async () => {
  await Promise.all(tempRoots.splice(0).map(root => rm(root, { recursive: true, force: true })))
})

describe('remote static cache policy', () => {
  it('accepts static frontend paths and versioned plugin bundles but never HTML/APIs', () => {
    expect(cachePathOf('/assets/index-8f3a2.js')).toBe('/assets/index-8f3a2.js')
    expect(cachePathOf('/assets/index-8f3a2.js?v=1')).toBeUndefined()
    expect(cachePathOf('/assets/nested/index.js')).toBeUndefined()
    expect(cachePathOf('/')).toBeUndefined()
    expect(cachePathOf('/index.html')).toBeUndefined()
    expect(cachePathOf('/api/session')).toBeUndefined()
    expect(cachePathOf('/plugins/client.js')).toBeUndefined()
    expect(cachePathOf('/plugins/??@deepseek-ai/dsh-client-modules/client.js&rev=abc'))
      .toBe('/plugins/??@deepseek-ai/dsh-client-modules/client.js&rev=abc')
    expect(cachePathOf('/manifest.webmanifest')).toBe('/manifest.webmanifest')
  })

  it('rejects authentication, private and content-negotiated responses', () => {
    expect(isCacheableResponse(200, { 'cache-control': 'max-age=300' })).toBe(true)
    for (const headers of [
      { 'set-cookie': ['session=secret'] },
      { 'cache-control': 'no-store' },
      { 'cache-control': 'private, max-age=300' },
      { vary: 'Cookie' },
      { vary: 'Authorization' },
    ])
      expect(isCacheableResponse(200, headers)).toBe(false)
    expect(isCacheableResponse(401, {})).toBe(false)
    expect(isCacheableResponse(302, {})).toBe(false)
  })
})

describe('authenticated response tee', () => {
  it('writes the same bytes delivered to the client and a sidecar metadata file', async () => {
    const root = await mkdtemp(join(tmpdir(), 'dsh-static-cache-'))
    tempRoots.push(root)
    const body = Buffer.from('export const frontend = "cached"\n')
    const server = createServer((_request, response) => {
      response.writeHead(200, { 'content-type': 'text/javascript' })
      response.end(body)
    })
    await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
    const client = createServer((_request, response) => {
      const address = server.address()
      if (address === null || typeof address === 'string')
        throw new Error('missing port')
      import('node:http').then(({ get }) => get(`http://127.0.0.1:${address.port}/assets/index.js`, (upstream) => {
        cacheBody(root, 'lan-host', 'http://192.168.31.83:3080', '/assets/index.js', upstream, response, { 'content-type': 'text/javascript' })
        upstream.pipe(response)
      })).catch(() => response.destroy())
    })
    await new Promise<void>(resolve => client.listen(0, '127.0.0.1', resolve))
    try {
      const address = client.address()
      if (address === null || typeof address === 'string')
        throw new Error('missing port')
      const reply = await fetch(`http://127.0.0.1:${address.port}/assets/index.js`)
      expect(await reply.text()).toBe(body.toString())
      await sleep(100)
      const key = createHash('sha256').update('/assets/index.js').digest('hex')
      const scope = createHash('sha256').update('http://192.168.31.83:3080').digest('hex').slice(0, 16)
      expect(await readFile(join(root, 'lan-host', scope, key), 'utf8')).toBe(body.toString())
      expect(JSON.parse(await readFile(join(root, 'lan-host', scope, `${key}.meta`), 'utf8'))).toMatchObject({ path: '/assets/index.js', contentType: 'text/javascript' })
      await expect(cachedAssetExists(root, 'lan-host', 'http://192.168.31.83:3080', '/assets/index.js')).resolves.toBe(true)
      await expect(cachedAssetExists(root, 'lan-host', 'https://dshs.inkmo.top', '/assets/index.js')).resolves.toBe(false)
    }
    finally {
      for (const current of [client, server])
        await new Promise<void>((resolve, reject) => current.close(error => error === undefined ? resolve() : reject(error)))
    }
  })

  it('isolates two hosts and rejects unsafe host identifiers', async () => {
    const root = await mkdtemp(join(tmpdir(), 'dsh-static-cache-'))
    tempRoots.push(root)
    const { PassThrough } = await import('node:stream')
    const first = new PassThrough()
    const second = new PassThrough()
    const response = new (class {
      private listeners: (() => void)[] = []
      once(_event: string, listener: () => void): void { this.listeners.push(listener) }
      off(_event: string, _listener: () => void): void {}
    })() as unknown as ServerResponse
    cacheBody(root, 'lan-host', 'http://192.168.31.83:3080', '/assets/index.js', first as unknown as IncomingMessage, response, {})
    cacheBody(root, 'https-host', 'https://dshs.inkmo.top', '/assets/index.js', second as unknown as IncomingMessage, response, {})
    first.end('lan')
    second.end('https')
    await sleep(100)
    const key = createHash('sha256').update('/assets/index.js').digest('hex')
    const lanScope = createHash('sha256').update('http://192.168.31.83:3080').digest('hex').slice(0, 16)
    const httpsScope = createHash('sha256').update('https://dshs.inkmo.top').digest('hex').slice(0, 16)
    expect(await readFile(join(root, 'lan-host', lanScope, key), 'utf8')).toBe('lan')
    expect(await readFile(join(root, 'https-host', httpsScope, key), 'utf8')).toBe('https')
    expect(await readdir(root)).toHaveLength(2)
  })

  it('drops older content of the same path after a new bundle lands', async () => {
    const root = await mkdtemp(join(tmpdir(), 'dsh-static-cache-'))
    tempRoots.push(root)
    const scope = createHash('sha256').update('http://192.168.31.83:3080').digest('hex').slice(0, 16)
    const key = createHash('sha256').update('/manifest.webmanifest').digest('hex')
    const directory = join(root, 'lan-host', scope)
    await mkdir(directory, { recursive: true })
    const oldBody = Buffer.alloc(10, 'a')
    const newBody = Buffer.alloc(10, 'b')
    const oldKey = 'f'.repeat(64)
    await writeFile(join(directory, oldKey), oldBody)
    await writeFile(join(directory, `${oldKey}.meta`), JSON.stringify({ path: '/manifest.webmanifest', fetchedAt: 1, contentType: 'application/manifest+json', bytes: 10, contentHash: createHash('sha256').update(oldBody).digest('hex') }), 'utf8')
    const { PassThrough } = await import('node:stream')
    const upstream = new PassThrough()
    let close: (() => void) | undefined
    const response = new (class {
      once(_event: string, listener: () => void): void { close = listener }
      off(_event: string, _listener: () => void): void {}
    })() as unknown as ServerResponse
    cacheBody(root, 'lan-host', 'http://192.168.31.83:3080', '/manifest.webmanifest', upstream as unknown as IncomingMessage, response, {})
    upstream.end(newBody)
    await sleep(150)
    close?.()
    await sleep(150)
    await expect(readFile(join(directory, key), 'utf8')).resolves.toBe(newBody.toString())
    await expect(readFile(join(directory, oldKey), 'utf8')).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('drops an interrupted response instead of caching a partial bundle', async () => {
    const root = await mkdtemp(join(tmpdir(), 'dsh-static-cache-'))
    tempRoots.push(root)
    const { PassThrough } = await import('node:stream')
    const upstream = new PassThrough()
    let close: (() => void) | undefined
    const response = new (class {
      once(_event: string, listener: () => void): void { close = listener }
      off(_event: string, _listener: () => void): void {}
    })() as unknown as ServerResponse
    cacheBody(root, 'lan-host', 'http://192.168.31.83:3080', '/assets/index.js', upstream as unknown as IncomingMessage, response, {})
    upstream.write('partial')
    close?.()
    upstream.end('truncated')
    await sleep(100)
    await expect(readdir(join(root, 'lan-host')).catch(() => [])).resolves.toEqual([])
  })
})
