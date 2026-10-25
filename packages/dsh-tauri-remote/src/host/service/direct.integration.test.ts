import type { AddressInfo } from 'node:net'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { afterEach, describe, expect, it } from 'vitest'
import { machineProfiles, machineStates, setHostConfig, setMachineDeps, setSourceSession } from '../config/runtime'
import { ConfigSchema } from '../config/schema'
import { MachineId } from '../types/index'
import { gateway } from './gateway'
import { machine } from './machine'

const id = MachineId('direct-test')
const home = await mkdtemp(join(tmpdir(), 'dsh-direct-machine-'))
setSourceSession('direct-integration')
setHostConfig(ConfigSchema())
setMachineDeps({
  transport: { resolve: () => { throw new Error('direct must not resolve SSH') } },
  emitStatus: () => undefined,
  localAllowlist: () => ({ allowBuilds: {}, onlyBuiltDependencies: [] }),
})

afterEach(async () => {
  await machine.dispose()
  await gateway.dispose()
  machineStates.clear()
  machineProfiles.clear()
})

describe('direct machine integration', () => {
  it('connects through the real gateway and caches an allowed static response', async () => {
    const cacheRoot = await mkdtemp(join(tmpdir(), 'dsh-direct-cache-'))
    const previousCache = process.env.INKDSH_REMOTE_CACHE_DIR
    process.env.INKDSH_REMOTE_CACHE_DIR = cacheRoot
    const upstream = createServer((request, response) => {
      if (request.url?.startsWith('/assets/')) {
        response.writeHead(200, { 'content-type': 'text/javascript', 'cache-control': 'max-age=300' })
        response.end('export const direct = true')
      }
      else {
        response.writeHead(200, { 'content-type': 'text/html' })
        response.end('<!doctype html><script src="/assets/index.js" type="module"></script>')
      }
    })
    await new Promise<void>(resolve => upstream.listen(0, '127.0.0.1', resolve))
    try {
      const origin = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`
      machineProfiles.set(id, { id, name: 'direct', host: '127.0.0.1', port: 80, user: '', remotePort: 80, transport: 'direct', directUrl: origin, directAuthMethod: 'token' })
      const link = await machine.connect(id)
      const page = await fetch(`${link.tunnelBaseUrl}/`)
      expect(page.status).toBe(200)
      const asset = await fetch(`${link.tunnelBaseUrl}/assets/index.js`)
      expect(await asset.text()).toBe('export const direct = true')
      expect(machine.status(id).state).toBe('connected')
      expect(machine.status(id).cacheIdentity).toBe(origin)
      const { createHash } = await import('node:crypto')
      const scope = createHash('sha256').update(origin).digest('hex').slice(0, 16)
      const key = createHash('sha256').update('/assets/index.js').digest('hex')
      await sleep(100)
      expect(await readFile(join(cacheRoot, id, scope, key), 'utf8')).toBe('export const direct = true')
      await machine.disconnect(id)
      expect(machine.status(id).state).toBe('disconnected')
    }
    finally {
      await new Promise<void>((resolve, reject) => upstream.close(error => error === undefined ? resolve() : reject(error)))
      if (previousCache === undefined)
        delete process.env.INKDSH_REMOTE_CACHE_DIR
      else
        process.env.INKDSH_REMOTE_CACHE_DIR = previousCache
      await Promise.all([home, cacheRoot].map(root => rm(root, { recursive: true, force: true })))
    }
  })

  it('logs in once through dsh-web-login and reuses its cookie for the page and assets', async () => {
    const requests: { url?: string, cookie?: string }[] = []
    let passwordPosts = 0
    const upstream = createServer((request, response) => {
      requests.push({ url: request.url, cookie: request.headers.cookie })
      if (request.url === '/__login' && request.method === 'POST') {
        passwordPosts += 1
        let body = ''
        request.setEncoding('utf8')
        request.on('data', (chunk) => {
          body += chunk
        })
        request.on('end', () => {
          const form = new URLSearchParams(body)
          if (form.get('password') !== 'host-password' || form.get('username') !== 'ink') {
            response.writeHead(401, { 'content-type': 'text/plain' })
            response.end('wrong credentials')
            return
          }
          response.writeHead(303, { 'location': '/', 'set-cookie': 'dsh_session=signed-session; HttpOnly; Path=/' })
          response.end()
        })
        return
      }
      if (request.headers.cookie !== 'dsh_session=signed-session') {
        response.writeHead(401, { 'content-type': 'text/plain' })
        response.end('login required')
        return
      }
      if (request.url?.startsWith('/assets/')) {
        response.writeHead(200, { 'content-type': 'text/javascript' })
        response.end('export const authenticated = true')
      }
      else {
        response.writeHead(200, { 'content-type': 'text/html' })
        response.end('<!doctype html><title>authenticated</title>')
      }
    })
    await new Promise<void>(resolve => upstream.listen(0, '127.0.0.1', resolve))
    try {
      const origin = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`
      machineProfiles.set(id, {
        id,
        name: 'direct',
        host: '127.0.0.1',
        port: 80,
        user: '',
        remotePort: 80,
        transport: 'direct',
        directUrl: origin,
        directAuthMethod: 'password',
        loginUsername: 'ink',
        password: 'host-password',
      })
      const link = await machine.connect(id)
      expect(passwordPosts).toBe(1)
      expect((await fetch(`${link.tunnelBaseUrl}/`)).status).toBe(200)
      expect((await fetch(`${link.tunnelBaseUrl}/assets/index.js`)).status).toBe(200)
      expect(passwordPosts).toBe(1)
      expect(requests.every(request => !request.url?.includes('host-password'))).toBe(true)
      const authenticated = requests.filter(request => request.cookie !== undefined)
      expect(authenticated.length).toBeGreaterThanOrEqual(2)
      expect(authenticated.every(request => request.cookie === 'dsh_session=signed-session'), JSON.stringify(requests)).toBe(true)
      expect(requests.some(request => request.url === '/assets/index.js' && request.cookie === 'dsh_session=signed-session')).toBe(true)
      await machine.disconnect(id)
    }
    finally {
      await new Promise<void>((resolve, reject) => upstream.close(error => error === undefined ? resolve() : reject(error)))
    }
  })
})
