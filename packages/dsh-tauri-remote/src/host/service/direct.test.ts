import type { MachineProfile } from '../types/index'
import { describe, expect, it } from 'vitest'
import { directAuthenticatedUrl, directBaseUrl, isDirect } from './direct'

function profile(extra: Partial<MachineProfile> = {}): MachineProfile {
  return {
    id: 'lan' as MachineProfile['id'],
    name: 'LAN',
    host: '192.168.31.83',
    port: 3080,
    user: '',
    remotePort: 3080,
    transport: 'direct',
    directUrl: 'http://192.168.31.83:3080',
    ...extra,
  }
}

describe('direct host configuration', () => {
  it('accepts both LAN HTTP and public HTTPS origins', () => {
    expect(directBaseUrl('http://192.168.31.83:3080/').origin).toBe('http://192.168.31.83:3080')
    expect(directBaseUrl('https://dshs.inkmo.top/').origin).toBe('https://dshs.inkmo.top')
    expect(isDirect(profile())).toBe(true)
    expect(isDirect(profile({ transport: 'ssh' }))).toBe(false)
  })

  it.each([
    'ftp://example.com/',
    'https://user:password@example.com/',
    'https://example.com/workspace',
    'https://example.com/?token=secret',
    'https://example.com/#secret',
  ])('rejects non-origin or credential-bearing direct URL %s', (url) => {
    expect(() => directBaseUrl(url)).toThrow()
  })

  it('keeps the token out of the configured URL and adds it only for authentication', () => {
    const configured = profile({ directUrl: 'https://dshs.inkmo.top', accessToken: 'secret-token' })
    expect(directBaseUrl(configured.directUrl).search).toBe('')
    const url = new URL(directAuthenticatedUrl(configured)!)
    expect(url.origin).toBe('https://dshs.inkmo.top')
    expect(url.searchParams.get('token')).toBe('secret-token')
    expect(directAuthenticatedUrl(profile())).toBeUndefined()
  })
})
