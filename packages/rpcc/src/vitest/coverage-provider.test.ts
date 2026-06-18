import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import * as fs from 'node:fs'

vi.mock('node:fs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs')>()
  return { ...actual, existsSync: vi.fn(), readFileSync: vi.fn(), readdirSync: vi.fn() }
})

const OID_MAP = JSON.stringify([
  { oid: 100, schema: 'public', name: 'my_func', args: '' },
])

// Field is "type" to match what the Rust instrumenter actually serializes
// (BranchLocation, #[serde(rename = "type")]). Using "kind" here previously
// masked the provider reading the wrong field.
const BRANCH_MAP = JSON.stringify({
  '100': {
    branches: {
      '1': { type: 'branch', line: 1 },
      '2': { type: 'branch', line: 2 },
      '3': { type: 'stmt', line: 3 },
    },
  },
})

const HITS_FILE = JSON.stringify({ hits: ['100|1'] })

describe('RpccCoverageProvider', () => {
  let provider: ReturnType<typeof import('./coverage-provider.js').getProvider>

  beforeEach(async () => {
    vi.resetModules()
    const mod = await import('./coverage-provider.js')
    provider = mod.getProvider()
  })

  afterEach(() => {
    vi.clearAllMocks()
  })

  it('has name "rpcc"', () => {
    expect(provider.name).toBe('rpcc')
  })

  it('default export has getProvider', async () => {
    vi.resetModules()
    const mod = await import('./coverage-provider.js')
    expect(typeof (mod as any).default?.getProvider).toBe('function')
  })

  describe('generateCoverage', () => {
    function setupFs(hits = HITS_FILE) {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readdirSync).mockReturnValue(['hits-1.json'] as any)
      vi.mocked(fs.readFileSync).mockImplementation((p: any) => {
        if (String(p).endsWith('oid_map.json')) return OID_MAP
        if (String(p).endsWith('branch_map.json')) return BRANCH_MAP
        if (String(p).endsWith('hits-1.json')) return hits
        return ''
      })
    }

    it('returns correct shape when hits files exist', () => {
      setupFs()
      const result = provider.generateCoverage(null) as any
      // 2 non-stmt branches total, 1 hit
      expect(result.total.branches).toEqual({ pct: 50, covered: 1, total: 2 })
      // 3 distinct lines (1, 2, 3), line 1 hit
      expect(result.total.lines.total).toBe(3)
      expect(result.total.lines.covered).toBe(1)
      expect(result.total.functions).toEqual({ pct: 100, covered: 1, total: 1 })
      expect(result.total.statements.pct).toBe(result.total.lines.pct)
    })

    it('excludes stmt entries from the branch denominator', () => {
      // Regression guard: the provider must key off the "type" field the Rust
      // instrumenter emits. Reading the wrong field made every stmt entry count
      // as a branch (denominator 3 instead of 2 here).
      setupFs()
      const result = provider.generateCoverage(null) as any
      expect(result.total.branches.total).toBe(2)
      expect(result.total.branches.total).not.toBe(3)
    })

    it('returns { total: null } when oid_map.json does not exist', () => {
      vi.mocked(fs.existsSync).mockReturnValue(false)
      const result = provider.generateCoverage(null) as any
      expect(result).toEqual({ total: null })
    })

    it('warns when oid_map.json missing', () => {
      vi.mocked(fs.existsSync).mockReturnValue(false)
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
      provider.generateCoverage(null)
      expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('[rpcc]'))
    })

    it('handles zero hits gracefully', () => {
      setupFs(JSON.stringify({ hits: [] }))
      const result = provider.generateCoverage(null) as any
      expect(result.total.branches.covered).toBe(0)
      expect(result.total.branches.pct).toBe(0)
    })
  })

  describe('reportCoverage', () => {
    it('logs coverage summary to stdout', () => {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readdirSync).mockReturnValue(['hits-1.json'] as any)
      vi.mocked(fs.readFileSync).mockImplementation((p: any) => {
        if (String(p).endsWith('oid_map.json')) return OID_MAP
        if (String(p).endsWith('branch_map.json')) return BRANCH_MAP
        if (String(p).endsWith('hits-1.json')) return HITS_FILE
        return ''
      })
      const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {})
      const coverage = provider.generateCoverage(null)
      provider.reportCoverage(coverage, null)
      expect(logSpy).toHaveBeenCalledWith(expect.stringContaining('lines'))
      expect(logSpy).toHaveBeenCalledWith(expect.stringContaining('branches'))
    })

    it('does not throw when coverage has no total', () => {
      expect(() => provider.reportCoverage({ total: null }, null)).not.toThrow()
    })
  })
})
