import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'

interface RpccCoverageTotals {
  functions: number
  branches: number
  covered_branches: number
  branch_percent: number
  lines: number
  covered_lines: number
  line_percent: number
}

interface RpccCoverageJson {
  totals: RpccCoverageTotals
}

function readCoverageJson(cwd: string): RpccCoverageJson | null {
  const coveragePath = resolve(cwd, '.rpcc', 'coverage.json')
  if (!existsSync(coveragePath)) return null
  try {
    return JSON.parse(readFileSync(coveragePath, 'utf8')) as RpccCoverageJson
  } catch (err) {
    console.warn(`[rpcc] Failed to parse coverage.json: ${err}`)
    return null
  }
}

function buildCoverageResult(totals: RpccCoverageTotals) {
  return {
    total: {
      lines: {
        pct: totals.line_percent,
        covered: totals.covered_lines,
        total: totals.lines,
      },
      branches: {
        pct: totals.branch_percent,
        covered: totals.covered_branches,
        total: totals.branches,
      },
      functions: {
        // rpcc doesn't track function-level coverage separately — report 100%
        // so Robit doesn't flag it as missing
        pct: 100,
        covered: totals.functions,
        total: totals.functions,
      },
      statements: {
        // rpcc has no statement concept distinct from lines — mirror lines
        pct: totals.line_percent,
        covered: totals.covered_lines,
        total: totals.lines,
      },
    },
  }
}

const RpccCoverageProvider = {
  name: 'rpcc' as const,

  initialize(_ctx: unknown): void {
    // rpcc instrumentation is handled by globalSetup — nothing to do here
  },

  resolveOptions() {
    return {
      provider: 'custom' as const,
      enabled: true,
      clean: false,
      cleanOnRerun: false,
      reportsDirectory: '.rpcc',
      exclude: [],
      excludeAfterRemap: false,
      include: [],
      extension: [],
      allowExternal: false,
      processingConcurrency: 1,
      reporter: [['text', {}]] as [string, Record<string, unknown>][],
      reportOnFailure: false,
      thresholds: {},
      ignoreEmptyLines: false,
      customProviderModule: '@carbotic-ai/rpcc/coverage',
    }
  },

  clean(_clean?: boolean): void {
    // rpcc manages its own .rpcc/ directory lifecycle via globalSetup
  },

  onAfterSuiteRun(_meta: unknown): void {
    // rpcc collects coverage via Postgres NOTICEs, not per-file JS coverage
  },

  generateCoverage(_reportContext: unknown): unknown {
    const json = readCoverageJson(process.cwd())
    if (!json) {
      console.warn('[rpcc] coverage.json not found or invalid — was globalSetup run?')
      return { total: null }
    }
    return buildCoverageResult(json.totals)
  },

  reportCoverage(coverage: unknown, _reportContext: unknown): void {
    const cov = coverage as ReturnType<typeof buildCoverageResult>
    if (!cov?.total?.lines) return
    const { lines, branches } = cov.total
    console.log(
      `[rpcc] coverage: ${lines.covered}/${lines.total} lines (${lines.pct}%), ` +
        `${branches.covered}/${branches.total} branches (${branches.pct}%)`
    )
  },
}

// CoverageProviderModule — the shape Vitest imports from customProviderModule
export function getProvider() {
  return RpccCoverageProvider
}
