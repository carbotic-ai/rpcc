#!/usr/bin/env node
import fs from 'node:fs'
import path from 'node:path'
import https from 'node:https'
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
const packageRoot = path.resolve(path.dirname(scriptPath), '..')
const binDir = path.join(packageRoot, 'bin')
ensureDir(binDir)

const binName = `rpcc-${platformKey()}${process.platform === 'win32' ? '.exe' : ''}`
const dest = path.join(binDir, binName)

if (fs.existsSync(dest)) {
  process.exit(0)
}

if (process.env.RPCC_SKIP_DOWNLOAD === '1') {
  console.log('rpcc: skipping binary download (RPCC_SKIP_DOWNLOAD=1)')
  process.exit(0)
}

const packageJsonPath = path.join(packageRoot, 'package.json')
const pkg = JSON.parse(fs.readFileSync(packageJsonPath, 'utf8'))
const version = pkg.version || '0.0.0'

const repoRoot = path.resolve(packageRoot, '..', '..')
const repoCargo = path.join(repoRoot, 'Cargo.toml')
const repoSql = path.join(repoRoot, 'sql', 'schema.sql')
if (fs.existsSync(repoCargo) && fs.existsSync(repoSql)) {
  console.log('rpcc: detected repo workspace; skipping binary download')
  process.exit(0)
}
const candidates = [
  path.join(repoRoot, 'target', 'release', 'rpcc-core'),
  path.join(repoRoot, 'target', 'debug', 'rpcc-core')
]

const source = candidates.find((p) => fs.existsSync(p))
if (source) {
  fs.copyFileSync(source, dest)
  if (process.platform !== 'win32') {
    fs.chmodSync(dest, 0o755)
  }
  console.log(`rpcc: installed ${dest}`)
  process.exit(0)
}

const base = process.env.RPCC_RELEASE_BASE || `https://github.com/carbotic-ai/rpcc/releases/download/v${version}`
const url = `${base}/${binName}`

download(url, dest)
  .then(() => {
    if (process.platform !== 'win32') {
      fs.chmodSync(dest, 0o755)
    }
    console.log(`rpcc: downloaded ${dest}`)
  })
  .catch((err) => {
    console.warn(`rpcc: failed to download ${url}`)
    console.warn(err?.message || err)
    console.warn('rpcc: set RPCC_BIN or build with `cargo build -p rpcc-core`')
    process.exit(1)
  })

function download(url, destPath) {
  return new Promise((resolve, reject) => {
    const request = https.get(
      url,
      { headers: { 'User-Agent': 'rpcc-postinstall' } },
      (res) => {
        if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume()
          download(res.headers.location, destPath).then(resolve).catch(reject)
          return
        }
        if (res.statusCode !== 200) {
          res.resume()
          reject(new Error(`unexpected status ${res.statusCode}`))
          return
        }
        const tmp = `${destPath}.tmp`
        const out = fs.createWriteStream(tmp)
        res.pipe(out)
        out.on('finish', () => {
          out.close(() => {
            fs.renameSync(tmp, destPath)
            resolve()
          })
        })
        out.on('error', (err) => {
          out.close(() => reject(err))
        })
      }
    )
    request.on('error', reject)
  })
}
