import { Pool, type PoolClient } from 'pg'
import { getRunId } from '../vitest/setup'

function validateIdentifier(name: string, label: string) {
  if (!/^[a-zA-Z0-9_.]+$/.test(name)) {
    throw new Error(`rpcc: invalid ${label} "${name}"`)
  }
}

function quoteQualifiedName(name: string): string {
  validateIdentifier(name, 'identifier')
  return name
    .split('.')
    .map((part) => `"${part}"`)
    .join('.')
}

export function createRpcTest(opts?: { connectionString?: string }) {
  const connectionString = opts?.connectionString || process.env.DATABASE_URL
  if (!connectionString) {
    throw new Error('rpcc: DATABASE_URL missing')
  }

  const pool = new Pool({
    connectionString,
    options: '-c client_min_messages=notice'
  })

  async function withTestTransaction<T>(fn: (client: PoolClient) => Promise<T>): Promise<T> {
    const client = await pool.connect()
    try {
      await client.query('BEGIN')

      const runId = getRunId()
      if (runId) {
        await client.query("SELECT set_config('rpcc.run_id', $1, true)", [runId])
      }

      await client.query('SELECT rpcc.reset_hits()')
      return await fn(client)
    } finally {
      await client.query('ROLLBACK')
      client.release()
    }
  }

  async function assertInTransaction(client: PoolClient) {
    const { rows } = await client.query<{ txid: string | null }>(
      'SELECT txid_current_if_assigned() AS txid'
    )
    if (!rows[0]?.txid) {
      throw new Error('rpcc: client is not in an explicit transaction')
    }
  }

  async function rpc<T = unknown>(
    fnName: string,
    args: Record<string, unknown> = {},
    client?: PoolClient
  ): Promise<T[]> {
    if (!client) {
      return withTestTransaction((tx) => rpc<T>(fnName, args, tx))
    }
    await assertInTransaction(client)
    validateIdentifier(fnName, 'function name')
    const keys = Object.keys(args)
    const placeholders = keys.map((key, idx) => `${key} := $${idx + 1}`).join(', ')
    const sql = `SELECT * FROM ${fnName}(${placeholders})`
    const values = keys.map((key) => args[key])
    const { rows } = await client.query(sql, values)
    return rows as T[]
  }

  async function seed(
    table: string,
    rows: Record<string, unknown>[],
    client?: PoolClient
  ): Promise<void> {
    if (!client) {
      await withTestTransaction((tx) => seed(table, rows, tx))
      return
    }
    await assertInTransaction(client)
    if (!rows.length) return
    const cols = Object.keys(rows[0])
    const values: unknown[] = []
    const valuePlaceholders = rows
      .map((row, rowIdx) => {
        const placeholders = cols.map((_, colIdx) => {
          values.push((row as Record<string, unknown>)[cols[colIdx]])
          return `$${rowIdx * cols.length + colIdx + 1}`
        })
        return `(${placeholders.join(',')})`
      })
      .join(',')

    const qualifiedTable = quoteQualifiedName(table)
    await client.query(
      `INSERT INTO ${qualifiedTable} (${cols.map((c) => `"${c}"`).join(',')}) VALUES ${valuePlaceholders}`,
      values
    )
  }

  return { rpc, seed, withTestTransaction, assertInTransaction, pool }
}
