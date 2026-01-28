import type { Plugin } from 'vite'
import { loadConfig } from '../config'
import { runInstrument, runRestore } from './setup'

export function rpccPlugin(opts?: { connectionString?: string; include?: string[]; exclude?: string[] }): Plugin {
  return {
    name: 'rpcc/vitest',
    async buildStart() {
      const cfg = loadConfig()
      const connectionString =
        opts?.connectionString || cfg.connectionString || process.env.DATABASE_URL
      runInstrument({ connectionString, include: opts?.include })
    },
    async closeBundle() {
      const cfg = loadConfig()
      const connectionString =
        opts?.connectionString || cfg.connectionString || process.env.DATABASE_URL
      runRestore({ connectionString })
    }
  }
}
