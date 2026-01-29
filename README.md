# rpcc

rpcc instruments Postgres PL/pgSQL functions to emit line and branch coverage signals at runtime.
It rewrites function bodies to call lightweight tracking helpers in the `rpcc` schema, then restores
original definitions after a run.

## What it does

- Injects coverage hooks into PL/pgSQL functions (line/statement and branch coverage).
- Emits `NOTICE` messages in a stable `rpcc|run_id|oid|branch` format during execution.
- Writes metadata to `.rpcc/` so coverage tooling can map hits back to function source.
- Restores original function definitions when the run finishes.

## How it works (high level)

1. `rpcc-core instrument` reads PL/pgSQL functions from `pg_proc`.
2. It rewrites the function body to include calls like `rpcc.track_line` and `rpcc.track_bool`.
3. It saves the original definition to `.rpcc/originals/` and writes maps to `.rpcc/`.
4. During tests, set `rpcc.run_id` and call `rpcc.reset_hits()` to start a clean run.
5. Coverage is collected by parsing `NOTICE` lines and joining to the maps in `.rpcc/`.
6. `rpcc-core restore` puts the original function definitions back.

## Repository layout

- `crates/rpcc-core/` — Rust CLI that instruments/restores functions.
- `sql/schema.sql` — SQL helpers (`rpcc.track*`, `rpcc.reset_hits`).
- `packages/rpcc/` — JS/TS helpers for Vitest and RPC testing.
- `tests/harness/` — Smoke test that exercises instrumentation and restore.

## Quick start (CLI)

1) Build the CLI:

```bash
cargo build -p rpcc-core
```

2) Install the SQL helpers in your database:

```bash
psql "$DATABASE_URL" -f sql/schema.sql
```

3) Instrument functions (examples):

```bash
# All PL/pgSQL functions in public schema
./target/debug/rpcc-core instrument --connection-string "$DATABASE_URL" --functions "public.*"

# Use SQL-like patterns (supports % and *)
./target/debug/rpcc-core instrument --connection-string "$DATABASE_URL" --functions "public.test_%"
```

4) Run your tests or queries with `rpcc.run_id` set (see JS helpers below), then restore:

```bash
./target/debug/rpcc-core restore --connection-string "$DATABASE_URL"
```

Useful flags:
- `--continue-on-error` keeps going if a function fails to instrument.
- `--dump-instrumented` writes rewritten SQL into `.rpcc/instrumented/`.
- `--dry-run` prints planned edits without applying them.

## Node/Vitest integration

The `packages/rpcc` package provides a Vitest plugin plus helpers to run RPC tests inside a
transaction with coverage enabled.

If you install from npm, a matching `rpcc-core` binary is downloaded automatically.
You can override the download URL with `RPCC_RELEASE_BASE` or provide your own binary via
`RPCC_BIN`.

Install dependencies in the repo root, then in your test suite:

```ts
import { defineConfig } from 'vitest/config'
import { rpccPlugin, globalSetup, globalTeardown } from 'rpcc'

export default defineConfig({
  plugins: [rpccPlugin({ include: ['public.my_function'] })],
  test: {
    globalSetup: [globalSetup],
    globalTeardown: [globalTeardown]
  }
})
```

Note: use **either** the plugin or the globalSetup/globalTeardown pair to manage
instrument/restore. Do not use both unless you intentionally want two passes.

### RPC test helper

```ts
import { createRpcTest } from 'rpcc'

const { rpc, seed } = createRpcTest({ connectionString: process.env.DATABASE_URL })

const rows = await rpc('public.get_users', { user_id: 123 })
await seed('public.Users', [{ id: 1, name: 'Ada' }])
```

The helper:
- Opens a transaction per call and rolls back afterward.
- Sets `rpcc.run_id` (if present) and resets hit tracking via `rpcc.reset_hits()`.

## Configuration

You can place `rpcc.config.json` in the project root:

```json
{
  "connectionString": "postgresql://...",
  "instrument": { "include": ["public.*"] }
}
```

`connectionString` defaults to `DATABASE_URL` if omitted.

### Environment variables

- `DATABASE_URL` — connection string for both CLI and JS helpers.
- `RPCC_BIN` — optional path to a specific `rpcc-core` binary.
- `RPCC_RELEASE_BASE` — optional base URL for binary downloads.
- `RPCC_RUN_ID` — optional override for the current run id.

## Output artifacts

Instrumentation creates a `.rpcc/` directory in the repo root:

- `.rpcc/session.json` — run id and status
- `.rpcc/originals/` — original function definitions
- `.rpcc/oid_map.json` — function metadata by OID
- `.rpcc/branch_map.json` — branch/line map
- `.rpcc/failures.json` — failures from `--continue-on-error`
- `.rpcc/instrumented/` — instrumented SQL (when `--dump-instrumented` is used)

## Restore and recovery

- `rpcc-core restore` restores functions from `.rpcc/originals/`.
- `rpcc-core recover` restores even if a prior session was interrupted.
- `rpcc-core status` prints the current `.rpcc/session.json` status.

## Limitations

- Only PL/pgSQL functions are instrumented.
- Dynamic SQL executed via `EXECUTE` is not instrumented internally; only the PL/pgSQL
  wrapper is tracked.
- Instrumentation rewrites function bodies, so you need permissions to replace functions
  and create the `rpcc` schema.

## Smoke test

`tests/harness/run.js` exercises instrumentation end-to-end. It expects a local Postgres
instance; update the connection string at the top of the file if needed.
