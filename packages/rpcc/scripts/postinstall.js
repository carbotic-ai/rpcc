#!/usr/bin/env node
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

function platformKey() {
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

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true })
}

const scriptPath = fileURLToPath(import.meta.url)
const binDir = path.resolve(path.dirname(scriptPath), '..', 'bin')
ensureDir(binDir)

const binName = `rpcc-${platformKey()}${process.platform === 'win32' ? '.exe' : ''}`
const dest = path.join(binDir, binName)

if (fs.existsSync(dest)) {
  process.exit(0)
}

const repoRoot = path.resolve(binDir, '..', '..', '..')
const candidates = [
  path.join(repoRoot, 'target', 'release', 'rpcc-core'),
  path.join(repoRoot, 'target', 'debug', 'rpcc-core')
]

const source = candidates.find((p) => fs.existsSync(p))
if (!source) {
  console.warn('rpcc: no local binary found; set RPCC_BIN or build with `cargo build -p rpcc-core`')
  process.exit(0)
}

fs.copyFileSync(source, dest)
if (process.platform !== 'win32') {
  fs.chmodSync(dest, 0o755)
}
console.log(`rpcc: installed ${dest}`)
