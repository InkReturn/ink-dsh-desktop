import type { MachineProfile, MachineTransport } from '../types/index'
import type { RemoteTransport } from './transport.types'
import { defineService } from 'dsh-tauri'
import { DEFAULT_MACHINE_TRANSPORT } from '../config/constants'
import { sshTransport } from './transport.ssh'

const IMPLEMENTATIONS: Partial<Record<MachineTransport, RemoteTransport>> = { ssh: sshTransport }

export const transport = defineService({
  resolve(profile: MachineProfile): RemoteTransport {
    const implementation = IMPLEMENTATIONS[profile.transport ?? DEFAULT_MACHINE_TRANSPORT]
    if (implementation === undefined)
      throw new TypeError('直连 Host 不提供 SSH transport；请使用 machine.connect')
    return implementation
  },
})
