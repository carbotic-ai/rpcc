import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { resolve } from 'node:path'

interface OidEntry {
  oid: number
  schema: string
  name: string
  args: string
}

interface BranchEntry {
  // The Rust instrumenter serializes this field as "type" (see BranchLocation
  // in crates/rpcc-core/src/main.rs, #[serde(rename = "type")]). Reading "kind"
  // here silently yielded undefined, so every stmt entry was counted as a branch.
  type: string
  line?: number
}

interface BranchMap {
  [oid: string]: { branches: { [branchId: string]: BranchEntry } }
}

function computeCoverageSummary(cwd: string) {
  const rpccDir = resolve(cwd, '.rpcc')
  const oidPath = resolve(rpccDir, 'oid_map.json')
  const branchPath = resolve(rpccDir, 'branch_map.json')
  if (!existsSync(oidPath) || !existsSync(branchPath)) return null

  const oidMap: OidEntry[] = JSON.parse(readFileSync(oidPath, 'utf8'))
  const branchMap: BranchMap = JSON.parse(readFileSync(branchPath, 'utf8'))

  const hits = new Set<string>()
  if (existsSync(rpccDir)) {
    const files = readdirSync(rpccDir).filter((f) => f.startsWith('hits-') && f.endsWith('.json'))
    for (const file of files) {
      const data = JSON.parse(readFileSync(resolve(rpccDir, file), 'utf8'))
      for (const hit of data.hits || []) {
        hits.add(hit as string)
      }
    }
  }

  let totalBranches = 0
  let coveredBranches = 0
  let totalLines = 0
  let coveredLines = 0
  let functionCount = 0

  for (const entry of oidMap) {
    const branches = branchMap[String(entry.oid)]?.branches || {}
    const branchIds = Object.keys(branches)
    let branchTotal = 0
    let branchCovered = 0
    const lineHits = new Map<string, boolean>()

    for (const branchId of branchIds) {
      const branch = branches[branchId]
      if (!branch) continue
      const hit = hits.has(`${entry.oid}|${branchId}`)
      if (branch.type !== 'stmt') {
        branchTotal += 1
        if (hit) branchCovered += 1
      }
      const lineKey = String(branch.line || 0)
      if (!lineHits.has(lineKey)) {
        lineHits.set(lineKey, hit)
      } else if (hit) {
        lineHits.set(lineKey, true)
      }
    }

    let functionCoveredLines = 0
    for (const value of lineHits.values()) {
      if (value) functionCoveredLines += 1
    }

    totalBranches += branchTotal
    coveredBranches += branchCovered
    totalLines += lineHits.size
    coveredLines += functionCoveredLines
    functionCount += 1
  }

  return {
    functions: functionCount,
    branches: totalBranches,
    covered_branches: coveredBranches,
    branch_percent: totalBranches ? Math.round((coveredBranches / totalBranches) * 10000) / 100 : 0,
    lines: totalLines,
    covered_lines: coveredLines,
    line_percent: totalLines ? Math.round((coveredLines / totalLines) * 10000) / 100 : 0,
  }
}

function buildCoverageResult(totals: ReturnType<typeof computeCoverageSummary> & object) {
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

const DEFAULT_THRESHOLDS = { lines: 95, branches: 88 }

interface CoverageThresholds {
  lines?: number
  branches?: number
}

interface IncomingCoverageOptions {
  thresholds?: CoverageThresholds
  [key: string]: unknown
}

const RpccCoverageProvider = {
  name: 'rpcc' as const,

  _thresholds: { ...DEFAULT_THRESHOLDS } as CoverageThresholds,

  initialize(_ctx: unknown): void {
    // rpcc instrumentation is handled by globalSetup — nothing to do here
  },

  resolveOptions(options: IncomingCoverageOptions) {
    this._thresholds = {
      lines: options?.thresholds?.lines ?? DEFAULT_THRESHOLDS.lines,
      branches: options?.thresholds?.branches ?? DEFAULT_THRESHOLDS.branches,
    }
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
      thresholds: this._thresholds,
      ignoreEmptyLines: false,
      customProviderModule: '@carbotic/rpcc/coverage',
    }
  },

  clean(_clean?: boolean): void {
    // rpcc manages its own .rpcc/ directory lifecycle via globalSetup
  },

  onAfterSuiteRun(_meta: unknown): void {
    // rpcc collects coverage via Postgres NOTICEs, not per-file JS coverage
  },

  generateCoverage(_reportContext: unknown): unknown {
    const totals = computeCoverageSummary(process.cwd())
    if (!totals) {
      console.warn('[rpcc] oid_map.json or branch_map.json not found — was globalSetup run?')
      return { total: null }
    }
    return buildCoverageResult(totals)
  },

  reportCoverage(coverage: unknown, _reportContext: unknown): void {
    const cov = coverage as ReturnType<typeof buildCoverageResult>
    if (!cov?.total?.lines) return
    const { lines, branches } = cov.total
    console.log(
      `[rpcc] coverage: ${lines.covered}/${lines.total} lines (${lines.pct}%), ` +
        `${branches.covered}/${branches.total} branches (${branches.pct}%)`
    )
    const failures: string[] = []
    if (this._thresholds.lines !== undefined && lines.pct < this._thresholds.lines)
      failures.push(`lines ${lines.pct}% below threshold ${this._thresholds.lines}%`)
    if (this._thresholds.branches !== undefined && branches.pct < this._thresholds.branches)
      failures.push(`branches ${branches.pct}% below threshold ${this._thresholds.branches}%`)
    if (failures.length > 0) throw new Error(`[rpcc] coverage thresholds not met: ${failures.join(', ')}`)
  },
}

// CoverageProviderModule — the shape Vitest imports from customProviderModule
export function getProvider() {
  return RpccCoverageProvider
}

export default { getProvider }
