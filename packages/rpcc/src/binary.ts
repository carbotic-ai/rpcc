import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

function platformKey(): string {
  const platform = process.platform
  const arch = process.arch
  if (platform === 'darwin') {
    return arch === 'arm64' ? 'darwin-arm64' : 'darwin-x64'
  }
  if (platform === 'linux') {
    return arch === 'arm64' ? 'linux-arm64' : 'linux-x64'
  }
  if (platform === 'win32') {
    return 'win32-x64'
  }
  return `${platform}-${arch}`
}

export function resolveRpccBin(): string {
  if (process.env.RPCC_BIN) {
    return process.env.RPCC_BIN
  }

  const filename = fileURLToPath(import.meta.url)
  const dirname = path.dirname(filename)
  const binDir = path.resolve(dirname, '..', 'bin')
  const binName = `rpcc-${platformKey()}${process.platform === 'win32' ? '.exe' : ''}`
  const packaged = path.join(binDir, binName)
  if (fs.existsSync(packaged)) {
    return packaged
  }

  // Local dev fallback to Rust build output
  const local = path.resolve(process.cwd(), 'target', 'debug', 'rpcc-core')
  if (fs.existsSync(local)) {
    return local
  }

  return 'rpcc'
}
