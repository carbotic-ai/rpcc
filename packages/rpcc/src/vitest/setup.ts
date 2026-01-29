import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { resolveRpccBin } from '../binary.js'
import { loadConfig } from '../config.js'

let currentRunId: string | null = null

export function setRunId(runId: string) {
  currentRunId = runId
  process.env.RPCC_RUN_ID = runId
}

export function getRunId(): string | null {
  if (process.env.RPCC_RUN_ID) {
    return process.env.RPCC_RUN_ID
  }
  if (currentRunId) {
    return currentRunId
  }
  const sessionPath = join(process.cwd(), '.rpcc', 'session.json')
  if (existsSync(sessionPath)) {
    const session = JSON.parse(readFileSync(sessionPath, 'utf8')) as { run_id?: string }
    if (session.run_id) {
      currentRunId = session.run_id
      return session.run_id
    }
  }
  return null
}

export function runInstrument(opts?: { connectionString?: string; include?: string[] }) {
  const cfg = loadConfig()
  const connectionString = opts?.connectionString || cfg.connectionString || process.env.DATABASE_URL
  if (!connectionString) {
    throw new Error('rpcc: connection string missing')
  }

  const include = opts?.include?.length ? opts.include : cfg.instrument?.include
  const bin = resolveRpccBin()
  const args = ['instrument', '--connection-string', connectionString]
  if (include?.length) {
    args.push('--functions', include.join(','))
  }

  const result = spawnSync(bin, args, { stdio: 'inherit' })
  if (result.status !== 0) {
    throw new Error('rpcc: instrument failed')
  }

  const sessionPath = join(process.cwd(), '.rpcc', 'session.json')
  if (!existsSync(sessionPath)) {
    throw new Error('rpcc: session.json missing after instrument')
  }
  const session = JSON.parse(readFileSync(sessionPath, 'utf8')) as { run_id?: string }
  if (!session.run_id) {
    throw new Error('rpcc: run_id missing from session.json')
  }
  setRunId(session.run_id)
  return session.run_id
}

export function runRestore(opts?: { connectionString?: string }) {
  const cfg = loadConfig()
  const connectionString = opts?.connectionString || cfg.connectionString || process.env.DATABASE_URL
  if (!connectionString) {
    return
  }
  const bin = resolveRpccBin()
  const result = spawnSync(bin, ['restore', '--connection-string', connectionString], {
    stdio: 'inherit'
  })
  if (result.status !== 0) {
    throw new Error('rpcc: restore failed')
  }
}

export default async function globalSetup() {
  runInstrument()
  return async () => {
    runRestore()
  }
}
