import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import * as fs from 'node:fs'
import * as path from 'node:path'

// Mock node:fs so we don't need real files on disk
vi.mock('node:fs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs')>()
  return { ...actual, existsSync: vi.fn(), readFileSync: vi.fn() }
})

const SAMPLE_COVERAGE_JSON = JSON.stringify({
  totals: {
    functions: 184,
    branches: 2973,
    covered_branches: 2192,
    branch_percent: 73.73,
    lines: 2404,
    covered_lines: 1847,
    line_percent: 76.83,
  },
})

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

  describe('generateCoverage', () => {
    it('returns correct Shape B when coverage.json exists', () => {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readFileSync).mockReturnValue(SAMPLE_COVERAGE_JSON)

      const result = provider.generateCoverage(null) as any

      expect(result.total.lines).toEqual({ pct: 76.83, covered: 1847, total: 2404 })
      expect(result.total.branches).toEqual({ pct: 73.73, covered: 2192, total: 2973 })
      expect(result.total.functions).toEqual({ pct: 100, covered: 184, total: 184 })
      expect(result.total.statements).toEqual({ pct: 76.83, covered: 1847, total: 2404 })
    })

    it('reads from .rpcc/coverage.json in process.cwd()', () => {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readFileSync).mockReturnValue(SAMPLE_COVERAGE_JSON)

      provider.generateCoverage(null)

      const expectedPath = path.resolve(process.cwd(), '.rpcc', 'coverage.json')
      expect(fs.existsSync).toHaveBeenCalledWith(expectedPath)
      expect(fs.readFileSync).toHaveBeenCalledWith(expectedPath, 'utf8')
    })

    it('returns { total: null } when coverage.json does not exist', () => {
      vi.mocked(fs.existsSync).mockReturnValue(false)

      const result = provider.generateCoverage(null) as any

      expect(result).toEqual({ total: null })
      expect(fs.readFileSync).not.toHaveBeenCalled()
    })

    it('returns { total: null } and warns when coverage.json is malformed', () => {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readFileSync).mockReturnValue('not valid json{{{')
      const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})

      const result = provider.generateCoverage(null) as any

      expect(result).toEqual({ total: null })
      expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('[rpcc] Failed to parse'))
    })
  })

  describe('reportCoverage', () => {
    it('logs coverage summary to stdout', () => {
      vi.mocked(fs.existsSync).mockReturnValue(true)
      vi.mocked(fs.readFileSync).mockReturnValue(SAMPLE_COVERAGE_JSON)
      const logSpy = vi.spyOn(console, 'log').mockImplementation(() => {})

      const coverage = provider.generateCoverage(null)
      provider.reportCoverage(coverage, null)

      expect(logSpy).toHaveBeenCalledWith(
        expect.stringContaining('1847/2404 lines (76.83%)')
      )
      expect(logSpy).toHaveBeenCalledWith(
        expect.stringContaining('2192/2973 branches (73.73%)')
      )
    })

    it('does not throw when coverage has no total', () => {
      expect(() => provider.reportCoverage({ total: null }, null)).not.toThrow()
    })
  })
})
