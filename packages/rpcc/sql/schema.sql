CREATE SCHEMA IF NOT EXISTS rpcc;

-- rpcc.track(oid, branch) - for PL/pgSQL branches
-- rpcc.track_line(oid, branch) - for statement/line coverage
--
-- These return boolean (always true) rather than void so the instrumenter can call
-- them via an assignment (rpcc_hit := rpcc.track(...)). An assignment preserves the
-- caller's FOUND and GET DIAGNOSTICS ROW_COUNT; a PERFORM would clobber both and
-- change the behavior of the instrumented function (e.g. break IF NOT FOUND).
--
-- Drop first: these previously returned void, and CREATE OR REPLACE cannot change a
-- function's return type. The DROPs make this schema safe to re-apply over an older install.
DROP FUNCTION IF EXISTS rpcc.track(oid, int);
DROP FUNCTION IF EXISTS rpcc.track_line(oid, int);

CREATE OR REPLACE FUNCTION rpcc.track(oid_val oid, branch int) RETURNS boolean AS $$
DECLARE
  run_id text := current_setting('rpcc.run_id', true);
  token text := '|' || oid_val::text || '|' || branch::text || '|';
  hits text;
BEGIN
  IF run_id IS NULL OR run_id = '' THEN
    RETURN true;
  END IF;

  IF current_setting('rpcc.dedup_disabled', true) = 'true' THEN
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN true;
  END IF;

  hits := coalesce(current_setting('rpcc.hits', true), ',');

  IF length(hits) > 500000 THEN
    RAISE WARNING 'rpcc: GUC size limit approaching, switching to non-dedup mode';
    PERFORM set_config('rpcc.dedup_disabled', 'true', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN true;
  END IF;

  IF position(token in hits) = 0 THEN
    PERFORM set_config('rpcc.hits', hits || token || ',', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
  END IF;

  RETURN true;
END;
$$ LANGUAGE plpgsql;

-- rpcc.track_line(oid, branch) - for statement/line coverage
CREATE OR REPLACE FUNCTION rpcc.track_line(oid_val oid, branch int) RETURNS boolean AS $$
DECLARE
  run_id text := current_setting('rpcc.run_id', true);
  token text := '|' || oid_val::text || '|' || branch::text || '|';
  hits text;
BEGIN
  IF run_id IS NULL OR run_id = '' THEN
    RETURN true;
  END IF;

  IF current_setting('rpcc.dedup_disabled', true) = 'true' THEN
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN true;
  END IF;

  hits := coalesce(current_setting('rpcc.hits', true), ',');

  IF length(hits) > 500000 THEN
    RAISE WARNING 'rpcc: GUC size limit approaching, switching to non-dedup mode';
    PERFORM set_config('rpcc.dedup_disabled', 'true', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN true;
  END IF;

  IF position(token in hits) = 0 THEN
    PERFORM set_config('rpcc.hits', hits || token || ',', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
  END IF;

  RETURN true;
END;
$$ LANGUAGE plpgsql;

-- rpcc.track_bool(oid, branch, val) - for SQL CASE WHEN (returns val unchanged)
CREATE OR REPLACE FUNCTION rpcc.track_bool(oid_val oid, branch int, val boolean) RETURNS boolean AS $$
DECLARE
  run_id text := current_setting('rpcc.run_id', true);
  token text := '|' || oid_val::text || '|' || branch::text || '|';
  hits text;
BEGIN
  IF val IS NOT TRUE OR run_id IS NULL OR run_id = '' THEN
    RETURN val;
  END IF;

  IF current_setting('rpcc.dedup_disabled', true) = 'true' THEN
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN val;
  END IF;

  hits := coalesce(current_setting('rpcc.hits', true), ',');

  IF length(hits) > 500000 THEN
    RAISE WARNING 'rpcc: GUC size limit approaching, switching to non-dedup mode';
    PERFORM set_config('rpcc.dedup_disabled', 'true', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN val;
  END IF;

  IF position(token in hits) = 0 THEN
    PERFORM set_config('rpcc.hits', hits || token || ',', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
  END IF;

  RETURN val;
END;
$$ LANGUAGE plpgsql;

-- rpcc.track_any(oid, branch, val) - for COALESCE (returns val unchanged)
CREATE OR REPLACE FUNCTION rpcc.track_any(oid_val oid, branch int, val anyelement) RETURNS anyelement AS $$
DECLARE
  run_id text := current_setting('rpcc.run_id', true);
  token text := '|' || oid_val::text || '|' || branch::text || '|';
  hits text;
BEGIN
  IF run_id IS NULL OR run_id = '' THEN
    RETURN val;
  END IF;

  IF current_setting('rpcc.dedup_disabled', true) = 'true' THEN
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN val;
  END IF;

  hits := coalesce(current_setting('rpcc.hits', true), ',');

  IF length(hits) > 500000 THEN
    RAISE WARNING 'rpcc: GUC size limit approaching, switching to non-dedup mode';
    PERFORM set_config('rpcc.dedup_disabled', 'true', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
    RETURN val;
  END IF;

  IF position(token in hits) = 0 THEN
    PERFORM set_config('rpcc.hits', hits || token || ',', true);
    RAISE NOTICE 'rpcc|%|%|%', run_id, oid_val, branch;
  END IF;

  RETURN val;
END;
$$ LANGUAGE plpgsql;

-- rpcc.reset_hits() - clear dedup state
CREATE OR REPLACE FUNCTION rpcc.reset_hits() RETURNS void AS $$
BEGIN
  PERFORM set_config('rpcc.hits', ',', true);
  PERFORM set_config('rpcc.dedup_disabled', '', true);
END;
$$ LANGUAGE plpgsql;
