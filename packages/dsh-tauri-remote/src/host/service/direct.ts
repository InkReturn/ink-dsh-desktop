import type { MachineId, MachineProfile, MachineSaveRow, RemoteLink } from '../types/index'
import { Buffer } from 'node:buffer'
import { request as httpRequest } from 'node:http'
import { request as httpsRequest } from 'node:https'

export const DIRECT_AUTH_REQUIRED = '直连 Host 需要登录密码或访问令牌；请在 Host 编辑里选择认证方式并填写凭据。'

export function directAuthMethodOf(profile: Pick<MachineProfile, 'directAuthMethod' | 'accessToken' | 'password'>): 'password' | 'token' {
  return profile.directAuthMethod ?? (profile.accessToken === undefined || profile.password !== undefined ? 'password' : 'token')
}

export function loginPathOf(profile: Pick<MachineProfile, 'loginPath'>): string {
  const path = profile.loginPath ?? '/__login'
  if (!/^\/[\w.~!$&'()*+,;=:@%/-]*$/u.test(path) || path === '/' || path.startsWith('/api') || path.includes('..'))
    throw new TypeError('登录路径必须是 /__login 这类同源路径，不能覆盖 / 或 /api')
  return path
}

export function validateDirectAuth(row: MachineSaveRow, secrets?: { password?: string, accessToken?: string }, existing?: MachineProfile): void {
  directBaseUrl(row.directUrl)
  const method = row.directAuthMethod ?? directAuthMethodOf(existing ?? {})
  if (method === 'password' && (secrets?.password ?? existing?.password ?? '') === '')
    throw new TypeError('密码登录需要 dsh-web-login 的登录密码；也可改选访问令牌')
  if (method === 'token' && (secrets?.accessToken ?? existing?.accessToken ?? '') === '')
    throw new TypeError('访问令牌不能为空；也可改选 dsh-web-login 密码登录')
  loginPathOf(row)
}

/** 只接受一个 HTTP(S) origin；不允许用户名、密码、路径、查询或 fragment 混入机器配置。 */
export function directBaseUrl(raw: string | undefined): URL {
  const url = new URL(raw ?? '')
  if (!['http:', 'https:'].includes(url.protocol) || url.username !== '' || url.password !== ''
    || (url.pathname !== '/' && url.pathname !== '') || url.search !== '' || url.hash !== '') {
    throw new TypeError('直连地址必须是 http(s)://主机[:端口]/，不能包含路径、查询或登录信息')
  }
  return url
}

export function isDirect(profile: Pick<MachineProfile, 'transport'>): boolean {
  return profile.transport === 'direct'
}

export function directAuthenticatedUrl(profile: MachineProfile): string | undefined {
  if (directAuthMethodOf(profile) !== 'token' || profile.accessToken === undefined || profile.accessToken === '')
    return undefined
  const url = directBaseUrl(profile.directUrl)
  url.searchParams.set('token', profile.accessToken)
  return url.toString()
}

/** 直连不做 SSH bootstrap：只探测前端，401 是「在线但需登录」，由 gateway 的 token 流程处理。 */
export async function probeDirect(profile: MachineProfile, signal?: AbortSignal): Promise<void> {
  const url = directBaseUrl(profile.directUrl)
  const send = url.protocol === 'https:' ? httpsRequest : httpRequest
  await new Promise<void>((resolve, reject) => {
    const req = send(url, { method: 'GET', headers: { accept: 'text/html' }, signal }, (response) => {
      response.resume()
      if ([200, 303, 401].includes(response.statusCode ?? 0))
        resolve()
      else
        reject(new Error(`直连 Host 前端返回 HTTP ${response.statusCode ?? 0}`))
    })
    req.setTimeout(8000, () => req.destroy(new Error('直连 Host 探测超时')))
    req.on('error', reject)
    req.end()
  })
}

/** 由 dsh-web-login 校验密码并签发会话 Cookie；桌面不复制口令哈希或校验逻辑。 */
export async function loginDirectCookie(profile: MachineProfile): Promise<string> {
  const base = directBaseUrl(profile.directUrl)
  const url = new URL(loginPathOf(profile), base)
  const body = new URLSearchParams({ next: '/', password: profile.password ?? '' })
  if ((profile.loginUsername ?? '') !== '')
    body.set('username', profile.loginUsername!)
  const payload = Buffer.from(body.toString())
  const send = base.protocol === 'https:' ? httpsRequest : httpRequest
  return await new Promise<string>((resolve, reject) => {
    const req = send(url, {
      method: 'POST',
      headers: {
        'accept': 'text/html',
        'content-type': 'application/x-www-form-urlencoded',
        'content-length': String(payload.length),
        'connection': 'close',
      },
    }, (response) => {
      const cookies = (response.headers['set-cookie'] ?? [])
        .map(value => value.split(';')[0]?.trim())
        .filter((value): value is string => value !== undefined && /^[^=;]+=[^;]+$/u.test(value))
      response.resume()
      if ((response.statusCode === 303 || response.statusCode === 302) && cookies.length > 0)
        resolve(cookies.join('; '))
      else if (response.statusCode === 401)
        reject(new Error('dsh-web-login 登录失败：密码或用户名不正确'))
      else if (response.statusCode === 429)
        reject(new Error('dsh-web-login 尝试次数过多，请稍后重试'))
      else if (response.statusCode === 404)
        reject(new Error('远端未启用 dsh-web-login；请安装/启用该插件，或改用访问令牌'))
      else
        reject(new Error(`dsh-web-login 登录失败（HTTP ${response.statusCode ?? 0}）`))
    })
    req.setTimeout(10_000, () => req.destroy(new Error('dsh-web-login 登录超时')))
    req.on('error', reject)
    req.end(payload)
  })
}

export async function connectDirect(machineId: MachineId, profile: MachineProfile, initialCookie?: string): Promise<RemoteLink> {
  await probeDirect(profile)
  const { gateway } = await import('./gateway')
  const entry = await gateway.start({
    id: `remote-outbound:${machineId}`,
    kind: 'outbound',
    upstream: directBaseUrl(profile.directUrl).origin,
    upstreamAuthority: true,
    cacheIdentity: directBaseUrl(profile.directUrl).origin,
    port: 0,
    tokenProvider: async () => directAuthenticatedUrl(profile),
    ...directAuthMethodOf(profile) === 'password' ? { cookieProvider: () => loginDirectCookie(profile) } : {},
    ...initialCookie === undefined ? {} : { initialCookie },
  })
  return { machineId, tunnelBaseUrl: `http://127.0.0.1:${entry.port}` }
}
