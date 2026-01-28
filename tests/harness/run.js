/* eslint-disable no-console */
const { Client } = require('pg');
const { spawnSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');
const CONN = 'postgresql://postgres:postgres@127.0.0.1:54322/postgres';
const RPCC_BIN = ['cargo', ['run', '-p', 'rpcc-core', '--']];

const functionsSql = `
DROP FUNCTION IF EXISTS test_dynamic_sql(text, text);
CREATE OR REPLACE FUNCTION test_static_case(p_type text, p_value int) RETURNS text AS $$
BEGIN
  RETURN CASE
    WHEN p_type = 'a' THEN
      CASE WHEN p_value > 10 THEN 'high-a' ELSE 'low-a' END
    WHEN p_type = 'b' THEN 'type-b'
    ELSE 'unknown'
  END;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION test_control_flow(p_input int) RETURNS text AS $$
BEGIN
  IF p_input IS NULL THEN
    RETURN 'null';
  ELSIF p_input < 0 THEN
    RETURN 'negative';
  ELSIF p_input = 0 THEN
    RETURN 'zero';
  ELSE
    RETURN 'positive';
  END IF;
EXCEPTION WHEN OTHERS THEN
  RETURN 'error';
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION test_dynamic_sql(p_column text, p_value text) RETURNS SETOF pg_class AS $$
DECLARE
  query text;
BEGIN
  IF p_column IS NULL THEN
    query := 'SELECT * FROM pg_class LIMIT 5';
  ELSE
    query := format('SELECT * FROM pg_class WHERE %I = %L LIMIT 5', p_column, p_value);
  END IF;

  RETURN QUERY EXECUTE query;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION test_unicode(p_emoji text) RETURNS text AS $$
BEGIN
  -- Comment with emoji: 🎉 and accented chars: café, naïve
  IF p_emoji = '🎉' THEN
    RETURN 'party';
  ELSIF p_emoji = '☕' THEN
    RETURN 'coffee';
  ELSE
    RETURN 'unknown: ' || p_emoji;
  END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION test_sql_expr(p_value int, p_fallback int) RETURNS int AS $$
BEGIN
  RETURN (
    SELECT COALESCE(NULLIF(p_value, 0), p_fallback)
  );
END;
$$ LANGUAGE plpgsql;
`;

async function main() {
  const rpccDir = path.join(ROOT, '.rpcc');
  if (fs.existsSync(rpccDir)) {
    fs.rmSync(rpccDir, { recursive: true, force: true });
  }

  const client = new Client({ connectionString: CONN, options: '-c client_min_messages=notice' });

  await client.connect();

  // Load schema
  const schemaSql = fs.readFileSync(path.join(ROOT, 'sql', 'schema.sql'), 'utf8');
  await client.query(schemaSql);

  // Create test functions
  await client.query(functionsSql);

  await client.end();

  // Run instrument
  runCmd(RPCC_BIN[0], [...RPCC_BIN[1], 'instrument', '--connection-string', CONN, '--functions', 'public.test_%']);

  const session = JSON.parse(fs.readFileSync(path.join(ROOT, '.rpcc', 'session.json'), 'utf8'));
  const runId = session.run_id;
  console.log('run_id:', runId);

  // Execute functions and capture notices
  const client2 = new Client({ connectionString: CONN, options: '-c client_min_messages=notice' });
  const notices = [];
  client2.on('notice', (msg) => {
    if (msg.message && msg.message.startsWith('rpcc|')) {
      notices.push(msg.message);
    }
  });
  await client2.connect();

  await client2.query('BEGIN');
  await client2.query("SELECT set_config('rpcc.run_id', $1, true)", [runId]);
  await client2.query('SELECT rpcc.reset_hits()');

  await client2.query("SELECT test_static_case('a', 5)");
  await client2.query("SELECT test_static_case('a', 20)");
  await client2.query("SELECT test_static_case('b', 1)");
  await client2.query("SELECT test_control_flow(NULL)");
  await client2.query("SELECT test_control_flow(-1)");
  await client2.query("SELECT test_control_flow(0)");
  await client2.query("SELECT test_control_flow(5)");
  await client2.query("SELECT test_unicode('🎉')");
  await client2.query("SELECT test_unicode('☕')");
  await client2.query('SELECT test_sql_expr(0, 42)');
  await client2.query('SELECT test_sql_expr(5, 42)');

  // Call dynamic SQL function returning pg_class rows
  await client2.query(`
    SELECT * FROM test_dynamic_sql(NULL, NULL)
    LIMIT 1;
  `);

  await client2.query('ROLLBACK');
  await client2.end();

  console.log('Captured NOTICEs:', notices.length);
  const formatOk = notices.every((n) => /^rpcc\|[a-z0-9-]+\|\d+\|\d+$/.test(n));
  if (!formatOk) {
    throw new Error('NOTICE format validation failed');
  }

  // Restore
  runCmd(RPCC_BIN[0], [...RPCC_BIN[1], 'restore', '--connection-string', CONN]);

  // Verify restore
  const client3 = new Client({ connectionString: CONN });
  await client3.connect();
  const def = await client3.query(
    "SELECT pg_get_functiondef(p.oid) AS def FROM pg_proc p WHERE p.proname = 'test_static_case'"
  );
  if (def.rows[0].def.includes('rpcc.track')) {
    throw new Error('Restore failed: instrumented code still present');
  }
  await client3.end();

  console.log('Harness completed successfully');
}

function runCmd(cmd, args) {
  const result = spawnSync(cmd, args, { stdio: 'inherit', cwd: ROOT });
  if (result.status !== 0) {
    throw new Error(`Command failed: ${cmd} ${args.join(' ')}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
