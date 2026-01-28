import fs from 'node:fs'
import path from 'node:path'

export interface RpccConfig {
  connectionString?: string
  instrument?: {
    include?: string[]
    exclude?: string[]
  }
  coverage?: {
    output?: string
    formats?: string[]
  }
}

export function defineConfig(config: RpccConfig): RpccConfig {
  return config
}

export function loadConfig(cwd = process.cwd()): RpccConfig {
  const configPath = path.join(cwd, 'rpcc.config.json')
  if (!fs.existsSync(configPath)) {
    return {}
  }
  return JSON.parse(fs.readFileSync(configPath, 'utf8')) as RpccConfig
}
