use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "rpcc")]
#[command(about = "rpcc instrumentation tool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Instrument {
        #[arg(long)]
        connection_string: String,
        #[arg(long)]
        functions: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        continue_on_error: bool,
        #[arg(long)]
        dump_instrumented: bool,
    },
    Restore {
        #[arg(long)]
        connection_string: String,
    },
    Status {
        #[arg(long)]
        connection_string: String,
    },
    Recover {
        #[arg(long)]
        connection_string: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Session {
    run_id: String,
    started_at: DateTime<Utc>,
    status: String,
}

#[derive(Debug)]
struct DbFunction {
    oid: u32,
    schema: String,
    name: String,
    args: String,
    xmin: String,
    definition: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct OidMapEntry {
    oid: u32,
    schema: String,
    name: String,
    args: String,
    xmin: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BranchLocation {
    #[serde(rename = "type")]
    kind: String,
    line: usize,
    col: usize,
    source: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BranchMapEntry {
    schema: String,
    name: String,
    args: String,
    branches: BTreeMap<String, BranchLocation>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstrumentFailure {
    oid: u32,
    schema: String,
    name: String,
    error: String,
}

#[derive(Debug, Clone)]
enum OpKind {
    Insert,
    Replace,
}

#[derive(Debug, Clone)]
struct Op {
    kind: OpKind,
    start: usize,
    end: usize,
    text: String,
}

#[derive(Debug)]
struct BodyParts {
    prefix: String,
    body: String,
    suffix: String,
}

#[derive(Debug)]
struct Token {
    start: usize,
    end: usize,
    upper: String,
}

struct BranchInject<'a> {
    body: &'a str,
    insert_pos: usize,
    source_pos: usize,
    oid: u32,
    branch_id: u32,
    kind: &'a str,
    ops: &'a mut Vec<Op>,
    branch_entry: &'a mut BranchMapEntry,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Instrument {
            connection_string,
            functions,
            dry_run,
            continue_on_error,
            dump_instrumented,
        } => {
            instrument_cmd(
                &connection_string,
                functions,
                dry_run,
                continue_on_error,
                dump_instrumented,
            )
            .await
        }
        Commands::Restore { connection_string } => restore_cmd(&connection_string).await,
        Commands::Status {
            connection_string: _,
        } => status_cmd().await,
        Commands::Recover { connection_string } => recover_cmd(&connection_string).await,
    }
}

async fn instrument_cmd(
    conn_str: &str,
    pattern: Option<String>,
    dry_run: bool,
    continue_on_error: bool,
    dump_instrumented: bool,
) -> Result<()> {
    let run_id = Uuid::new_v4().to_string();
    let started_at: DateTime<Utc> = Utc::now();

    let session = Session {
        run_id: run_id.clone(),
        started_at,
        status: "instrumenting".to_string(),
    };

    let rpcc_dir = ensure_rpcc_dir()?;
    write_session(&rpcc_dir, &session)?;

    let client = connect_db(conn_str).await?;
    ensure_session_table(&client).await?;
    upsert_db_session(&client, &session).await?;
    acquire_lock(&client).await?;

    let patterns: Vec<String> = pattern
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let result = instrument_with_client(
        &client,
        &rpcc_dir,
        &patterns,
        dry_run,
        continue_on_error,
        dump_instrumented,
    )
    .await;
    let session = Session {
        run_id,
        started_at,
        status: "testing".to_string(),
    };
    write_session(&rpcc_dir, &session)?;
    upsert_db_session(&client, &session).await?;
    release_lock(&client).await?;

    result
}

async fn instrument_with_client(
    client: &Client,
    rpcc_dir: &Path,
    patterns: &[String],
    dry_run: bool,
    continue_on_error: bool,
    dump_instrumented: bool,
) -> Result<()> {
    let functions = fetch_functions(client, patterns).await?;
    if functions.is_empty() {
        eprintln!("No functions matched pattern");
        return Ok(());
    }

    let mut oid_map: Vec<OidMapEntry> = Vec::new();
    let mut branch_map: BTreeMap<String, BranchMapEntry> = BTreeMap::new();
    let mut failures: Vec<InstrumentFailure> = Vec::new();

    let instrumented_dir = rpcc_dir.join("instrumented");
    if dump_instrumented {
        fs::create_dir_all(&instrumented_dir)?;
    }

    for func in functions {
        let filename = format!("{}.{}__{}.sql", func.schema, func.name, func.oid);
        let original_path = rpcc_dir.join("originals").join(filename);
        if is_already_instrumented(&func.definition) {
            let error = format!(
                "Failed to instrument {}.{}: function appears already instrumented; restore first",
                func.schema, func.name
            );
            failures.push(InstrumentFailure {
                oid: func.oid,
                schema: func.schema.clone(),
                name: func.name.clone(),
                error,
            });
            if !continue_on_error {
                return Err(anyhow!(
                    "Function {}.{} appears already instrumented; restore first",
                    func.schema,
                    func.name
                ));
            }
            continue;
        }

        fs::write(&original_path, func.definition.as_bytes())?;

        let (instrumented, branch_entry, plan) = instrument_function(&func)?;

        if dump_instrumented {
            let instrumented_path =
                instrumented_dir.join(format!("{}.{}__{}.sql", func.schema, func.name, func.oid));
            fs::write(&instrumented_path, instrumented.as_bytes())?;
        }

        if dry_run {
            println!("-- {}.{} (OID {})", func.schema, func.name, func.oid);
            for op in plan {
                println!(
                    "[{:?}] {}..{} => {}",
                    op.kind,
                    op.start,
                    op.end,
                    op.text.trim()
                );
            }
            oid_map.push(OidMapEntry {
                oid: func.oid,
                schema: func.schema.clone(),
                name: func.name.clone(),
                args: func.args.clone(),
                xmin: func.xmin.clone(),
            });
            branch_map.insert(func.oid.to_string(), branch_entry);
            continue;
        }

        if let Err(err) = client.batch_execute(&instrumented).await {
            let error = format!("Failed to instrument {}.{}: {err}", func.schema, func.name);
            failures.push(InstrumentFailure {
                oid: func.oid,
                schema: func.schema.clone(),
                name: func.name.clone(),
                error,
            });
            if !continue_on_error {
                return Err(err).with_context(|| {
                    format!("Failed to instrument {}.{}", func.schema, func.name)
                });
            }
            continue;
        }

        let instrumented_xmin = fetch_xmin(client, func.oid)
            .await
            .unwrap_or(func.xmin.clone());
        oid_map.push(OidMapEntry {
            oid: func.oid,
            schema: func.schema.clone(),
            name: func.name.clone(),
            args: func.args.clone(),
            xmin: instrumented_xmin,
        });
        branch_map.insert(func.oid.to_string(), branch_entry);
    }

    write_json(&rpcc_dir.join("oid_map.json"), &oid_map)?;
    write_json(&rpcc_dir.join("branch_map.json"), &branch_map)?;

    if !failures.is_empty() {
        write_json(&rpcc_dir.join("failures.json"), &failures)?;
        eprintln!("rpcc: {} functions failed to instrument", failures.len());
        if !continue_on_error {
            return Err(anyhow!("Instrumentation failed"));
        }
    }

    Ok(())
}

async fn restore_cmd(conn_str: &str) -> Result<()> {
    let rpcc_dir = ensure_rpcc_dir()?;
    let originals_dir = rpcc_dir.join("originals");

    let client = connect_db(conn_str).await?;
    ensure_session_table(&client).await?;
    acquire_lock(&client).await?;

    let result = restore_with_client(&client, &rpcc_dir, &originals_dir).await;
    release_lock(&client).await?;

    result
}

async fn status_cmd() -> Result<()> {
    let rpcc_dir = ensure_rpcc_dir()?;
    let session_path = rpcc_dir.join("session.json");
    if !session_path.exists() {
        println!("No rpcc session found");
        return Ok(());
    }
    let session: Session = serde_json::from_str(&fs::read_to_string(&session_path)?)?;
    println!("run_id: {}", session.run_id);
    println!("status: {}", session.status);
    println!("started_at: {}", session.started_at);
    Ok(())
}

async fn recover_cmd(conn_str: &str) -> Result<()> {
    let rpcc_dir = ensure_rpcc_dir()?;
    let originals_dir = rpcc_dir.join("originals");
    if !originals_dir.exists() {
        println!("No originals found; nothing to recover");
        return Ok(());
    }

    let client = connect_db(conn_str).await?;
    ensure_session_table(&client).await?;
    acquire_lock(&client).await?;

    let result = restore_with_client(&client, &rpcc_dir, &originals_dir).await;
    release_lock(&client).await?;
    result
}

async fn restore_with_client(client: &Client, rpcc_dir: &Path, originals_dir: &Path) -> Result<()> {
    let oid_map = load_oid_map(rpcc_dir)?;
    let mut restored = 0;
    for entry in fs::read_dir(originals_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        if let Some(oid) = parse_oid_from_filename(&path) {
            if let Some(entry) = oid_map.get(&oid) {
                let current_xmin = fetch_xmin(client, oid)
                    .await
                    .with_context(|| format!("Failed to query xmin for oid {oid}"))?;
                if current_xmin != entry.xmin {
                    let current_def = fetch_definition(client, oid).await.unwrap_or_default();
                    if !current_def.is_empty() && is_already_instrumented(&current_def) {
                        eprintln!(
                            "Warning: restoring instrumented {}.{} despite xmin mismatch ({} != {})",
                            entry.schema, entry.name, current_xmin, entry.xmin
                        );
                    } else {
                        continue;
                    }
                }
            }
        }
        let sql = fs::read_to_string(&path)?;
        client
            .batch_execute(&sql)
            .await
            .with_context(|| format!("Failed to restore {path:?}"))?;
        restored += 1;
    }
    let session_path = rpcc_dir.join("session.json");
    if session_path.exists() {
        let mut session: Session = serde_json::from_str(&fs::read_to_string(&session_path)?)?;
        session.status = "complete".to_string();
        write_session(rpcc_dir, &session)?;
        upsert_db_session(client, &session).await?;
    }

    println!("Restored {restored} functions");
    Ok(())
}

fn ensure_rpcc_dir() -> Result<PathBuf> {
    let root = std::env::current_dir()?;
    let rpcc_dir = root.join(".rpcc");
    let originals_dir = rpcc_dir.join("originals");
    fs::create_dir_all(&originals_dir)?;
    Ok(rpcc_dir)
}

fn load_oid_map(rpcc_dir: &Path) -> Result<HashMap<u32, OidMapEntry>> {
    let path = rpcc_dir.join("oid_map.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(path)?;
    let entries: Vec<OidMapEntry> = serde_json::from_str(&content)?;
    Ok(entries
        .into_iter()
        .map(|entry| (entry.oid, entry))
        .collect())
}

fn parse_oid_from_filename(path: &Path) -> Option<u32> {
    let stem = path.file_stem()?.to_string_lossy();
    let (_, oid_str) = stem.rsplit_once("__")?;
    oid_str.parse().ok()
}

fn write_session(rpcc_dir: &Path, session: &Session) -> Result<()> {
    let session_path = rpcc_dir.join("session.json");
    write_json(&session_path, session)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = fs::File::create(path)?;
    let content = serde_json::to_string_pretty(value)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

async fn connect_db(conn_str: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(conn_str, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("Postgres connection error: {err}");
        }
    });
    Ok(client)
}

async fn ensure_session_table(client: &Client) -> Result<()> {
    client
        .batch_execute(
            r#"
CREATE SCHEMA IF NOT EXISTS rpcc;
CREATE TABLE IF NOT EXISTS rpcc.session (
  id text PRIMARY KEY,
  started_at timestamptz,
  status text
);
"#,
        )
        .await?;
    Ok(())
}

async fn upsert_db_session(client: &Client, session: &Session) -> Result<()> {
    client
        .query(
            r#"
INSERT INTO rpcc.session (id, started_at, status)
VALUES ($1, $2, $3)
ON CONFLICT (id) DO UPDATE
SET started_at = EXCLUDED.started_at,
    status = EXCLUDED.status
"#,
            &[&session.run_id, &session.started_at, &session.status],
        )
        .await?;
    Ok(())
}

async fn acquire_lock(client: &Client) -> Result<()> {
    client
        .query("SELECT pg_advisory_lock(hashtext('rpcc_instrument'))", &[])
        .await?;
    Ok(())
}

async fn release_lock(client: &Client) -> Result<()> {
    client
        .query(
            "SELECT pg_advisory_unlock(hashtext('rpcc_instrument'))",
            &[],
        )
        .await?;
    Ok(())
}

async fn fetch_functions(client: &Client, patterns: &[String]) -> Result<Vec<DbFunction>> {
    let rows = client
        .query(
            "SELECT p.oid::bigint as oid,
                    n.nspname as schema,
                    p.proname as name,
                    pg_get_function_identity_arguments(p.oid) as args,
                    pg_get_functiondef(p.oid) as definition,
                    p.xmin::text as xmin,
                    l.lanname as language
             FROM pg_proc p
             JOIN pg_namespace n ON p.pronamespace = n.oid
             JOIN pg_language l ON p.prolang = l.oid
             WHERE n.nspname NOT IN ('pg_catalog','information_schema')
               AND l.lanname = 'plpgsql'",
            &[],
        )
        .await?;

    let mut result = Vec::new();
    for row in rows {
        let oid: i64 = row.get("oid");
        let schema: String = row.get("schema");
        let name: String = row.get("name");
        let args: String = row.get("args");
        let definition: String = row.get("definition");
        let xmin: String = row.get("xmin");

        if !patterns.is_empty()
            && !patterns
                .iter()
                .any(|pattern| matches_pattern(&schema, &name, pattern))
        {
            continue;
        }

        result.push(DbFunction {
            oid: oid as u32,
            schema,
            name,
            args,
            xmin,
            definition,
        });
    }

    Ok(result)
}

async fn fetch_xmin(client: &Client, oid: u32) -> Result<String> {
    let row = client
        .query_one(
            "SELECT xmin::text as xmin FROM pg_proc WHERE oid = $1",
            &[&oid],
        )
        .await?;
    let xmin: String = row.get("xmin");
    Ok(xmin)
}

async fn fetch_definition(client: &Client, oid: u32) -> Result<String> {
    let row = client
        .query_one("SELECT pg_get_functiondef($1::oid) AS def", &[&oid])
        .await?;
    let def: String = row.get("def");
    Ok(def)
}

fn matches_pattern(schema: &str, name: &str, pattern: &str) -> bool {
    let pattern = pattern.replace('*', "%");
    if let Some((schema_pat, name_pat)) = pattern.split_once('.') {
        let schema_re = like_to_regex(schema_pat);
        let name_re = like_to_regex(name_pat);
        return schema_re.is_match(schema) && name_re.is_match(name);
    }
    let name_re = like_to_regex(&pattern);
    name_re.is_match(name)
}

fn like_to_regex(pattern: &str) -> Regex {
    let escaped = regex::escape(pattern).replace('%', ".*");
    Regex::new(&format!("^{escaped}$")).unwrap()
}

fn is_already_instrumented(definition: &str) -> bool {
    definition.contains("rpcc.track(")
        || definition.contains("rpcc.track_line(")
        || definition.contains("rpcc.track_bool(")
        || definition.contains("rpcc.track_any(")
}

fn code_mask(body: &str) -> Vec<bool> {
    let bytes = body.as_bytes();
    let mut mask = vec![false; bytes.len()];
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let end = i.min(bytes.len());
            if start < end {
                mask[start..end].fill(true);
            }
            continue;
        }

        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
            let end = i.min(bytes.len());
            if start < end {
                mask[start..end].fill(true);
            }
            continue;
        }

        if bytes[i] == b'\'' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            let end = i.min(bytes.len());
            if start < end {
                mask[start..end].fill(true);
            }
            continue;
        }

        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            let end = i.min(bytes.len());
            if start < end {
                mask[start..end].fill(true);
            }
            continue;
        }

        if bytes[i] == b'$' {
            let rest = &body[i + 1..];
            if let Some(tag_end) = rest.find('$') {
                let tag = &rest[..tag_end];
                if tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    let end_tag = format!("${tag}$");
                    let search_start = i + 1 + tag_end + 1;
                    if let Some(end_idx) = body[search_start..].find(&end_tag) {
                        let end = search_start + end_idx + end_tag.len();
                        let end = end.min(bytes.len());
                        if i < end {
                            mask[i..end].fill(true);
                        }
                        i = end;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }

    mask
}

fn tokenize(body: &str, mask: &[bool]) -> Vec<Token> {
    let bytes = body.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let ch = bytes[i] as char;
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if mask.get(i).copied().unwrap_or(false) {
                    break;
                }
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let text = &body[start..i];
            tokens.push(Token {
                start,
                end: i,
                upper: text.to_uppercase(),
            });
            continue;
        }
        i += 1;
    }
    tokens
}

fn previous_non_ws_char(body: &str, mask: &[bool], pos: usize) -> Option<char> {
    let bytes = body.as_bytes();
    let mut i = pos;
    while i > 0 {
        i -= 1;
        if mask.get(i).copied().unwrap_or(false) {
            continue;
        }
        let ch = bytes[i] as char;
        if ch.is_whitespace() {
            continue;
        }
        return Some(ch);
    }
    None
}

fn is_stmt_start(body: &str, mask: &[bool], tokens: &[Token], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }

    let prev = &tokens[idx - 1];
    let upper = prev.upper.as_str();
    if upper == "EXCEPTION" {
        if idx >= 2 && tokens[idx - 2].upper == "RAISE" {
            // RAISE EXCEPTION ... has argument list; don't inject mid-statement
        } else {
            return true;
        }
    }

    if matches!(upper, "THEN" | "ELSE" | "LOOP" | "BEGIN") {
        return true;
    }

    matches!(
        previous_non_ws_char(body, mask, tokens[idx].start),
        Some(';')
    )
}

fn line_bounds(body: &str, pos: usize) -> (usize, usize) {
    let bytes = body.as_bytes();
    let mut start = pos;
    while start > 0 {
        if bytes[start - 1] == b'\n' {
            break;
        }
        start -= 1;
    }
    let mut end = pos;
    while end < bytes.len() {
        if bytes[end] == b'\n' {
            end += 1;
            break;
        }
        end += 1;
    }
    (start, end)
}

fn next_unmasked_semicolon(body: &str, mask: &[bool], start: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    for (idx, byte) in bytes.iter().enumerate().skip(start) {
        if mask.get(idx).copied().unwrap_or(false) {
            continue;
        }
        if *byte == b';' {
            return Some(idx);
        }
    }
    None
}

fn has_non_ws_between(body: &str, mask: &[bool], start: usize, end: usize) -> bool {
    let bytes = body.as_bytes();
    for (idx, byte) in bytes.iter().enumerate().take(end).skip(start) {
        if mask.get(idx).copied().unwrap_or(false) {
            continue;
        }
        if !(*byte as char).is_whitespace() {
            return true;
        }
    }
    false
}

fn is_plpgsql_end_case(
    body: &str,
    mask: &[bool],
    tokens: &[Token],
    idx: usize,
    limit_pos: usize,
) -> bool {
    if tokens[idx].upper.as_str() != "END" || tokens[idx + 1].upper.as_str() != "CASE" {
        return false;
    }
    if has_non_ws_between(body, mask, tokens[idx].end, tokens[idx + 1].start) {
        return false;
    }
    if let Some(next) = tokens.get(idx + 2) {
        if next.start < limit_pos {
            return false;
        }
    }
    true
}

fn has_end_case_before(
    body: &str,
    mask: &[bool],
    tokens: &[Token],
    start_idx: usize,
    limit_pos: usize,
) -> bool {
    let mut idx = start_idx + 1;
    while idx + 1 < tokens.len() {
        if tokens[idx].start >= limit_pos {
            return false;
        }
        if is_plpgsql_end_case(body, mask, tokens, idx, limit_pos) {
            return true;
        }
        idx += 1;
    }
    false
}

fn sql_statement_ranges(body: &str, mask: &[bool], tokens: &[Token]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut idx = 0usize;
    while idx < tokens.len() {
        let token = &tokens[idx];
        if is_stmt_start(body, mask, tokens, idx) {
            let upper = token.upper.as_str();
            if matches!(
                upper,
                "SELECT"
                    | "INSERT"
                    | "UPDATE"
                    | "DELETE"
                    | "WITH"
                    | "PERFORM"
                    | "RETURN"
                    | "EXECUTE"
            ) {
                if let Some(end) = next_unmasked_semicolon(body, mask, token.start) {
                    ranges.push((token.start, end + 1));
                    while idx < tokens.len() && tokens[idx].start < end {
                        idx += 1;
                    }
                    continue;
                }
            }
            if upper == "FOR" {
                if let Some((select_start, loop_start)) =
                    find_for_in_query_range(body, mask, tokens, idx)
                {
                    ranges.push((select_start, loop_start));
                    while idx < tokens.len() && tokens[idx].start < loop_start {
                        idx += 1;
                    }
                    continue;
                }
            }
        }
        idx += 1;
    }
    ranges
}

fn find_for_in_query_range(
    body: &str,
    mask: &[bool],
    tokens: &[Token],
    for_idx: usize,
) -> Option<(usize, usize)> {
    let stmt_end = next_unmasked_semicolon(body, mask, tokens[for_idx].start).unwrap_or(body.len());
    let mut in_idx = None;
    let mut select_idx = None;
    let mut loop_idx = None;

    for idx in (for_idx + 1)..tokens.len() {
        let token = &tokens[idx];
        if token.start >= stmt_end {
            break;
        }
        if in_idx.is_none() && token.upper.as_str() == "IN" {
            in_idx = Some(idx);
            continue;
        }
        if in_idx.is_some() {
            if select_idx.is_none() && matches!(token.upper.as_str(), "SELECT" | "WITH" | "EXECUTE")
            {
                select_idx = Some(idx);
                continue;
            }
            if token.upper.as_str() == "LOOP" && is_stmt_start(body, mask, tokens, idx) {
                loop_idx = Some(idx);
                break;
            }
        }
    }

    let select_idx = select_idx?;
    let loop_idx = loop_idx?;
    let select_start = tokens[select_idx].start;
    let loop_start = tokens[loop_idx].start;
    if select_start >= loop_start {
        None
    } else {
        Some((select_start, loop_start))
    }
}

fn is_in_ranges(pos: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| pos >= *start && pos < *end)
}

fn is_stmt_skip_keyword(upper: &str) -> bool {
    matches!(
        upper,
        "BEGIN" | "END" | "ELSE" | "ELSIF" | "EXCEPTION" | "DECLARE" | "WHEN" | "THEN"
    )
}

fn find_statement_lines(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();
    let mut declare_depth = 0usize;
    let mut expr_case_depth = 0usize;

    for (idx, token) in tokens.iter().enumerate() {
        let upper = token.upper.as_str();

        if upper == "DECLARE" && is_stmt_start(body, &mask, &tokens, idx) {
            declare_depth += 1;
            continue;
        }

        if upper == "BEGIN" && is_stmt_start(body, &mask, &tokens, idx) {
            if declare_depth > 0 {
                declare_depth = declare_depth.saturating_sub(1);
            }
            continue;
        }

        if declare_depth > 0 {
            continue;
        }

        if upper == "CASE" && !is_stmt_start(body, &mask, &tokens, idx) {
            expr_case_depth += 1;
            continue;
        }

        if upper == "END" && expr_case_depth > 0 {
            let next_upper = tokens.get(idx + 1).map(|t| t.upper.as_str());
            if next_upper != Some("CASE") {
                expr_case_depth = expr_case_depth.saturating_sub(1);
                continue;
            }
        }

        if expr_case_depth > 0 {
            continue;
        }

        if !is_stmt_start(body, &mask, &tokens, idx) {
            continue;
        }

        if is_stmt_skip_keyword(upper) {
            continue;
        }

        if let Some((range_start, range_end)) = sql_ranges
            .iter()
            .find(|(start, end)| token.start >= *start && token.start < *end)
        {
            if token.start != *range_start {
                continue;
            }
            if *range_end <= token.start {
                continue;
            }
        }

        let current_id = *branch_id;
        *branch_id += 1;

        let (line_start, line_end) = line_bounds(body, token.start);
        let line_text = body[line_start..line_end].trim_end().to_string();

        let indent_slice = &body[line_start..token.start];
        let indent_is_ws = indent_slice.chars().all(|c| c.is_whitespace());

        let insert_pos = if indent_is_ws {
            line_start
        } else {
            token.start
        };
        let (line, col) = line_col_from_offset(body, insert_pos);
        let injection = if indent_is_ws {
            format!("{indent_slice}{}\n", track_call("rpcc.track_line", oid, current_id))
        } else {
            format!("{} ", track_call("rpcc.track_line", oid, current_id))
        };

        ops.push(Op {
            kind: OpKind::Insert,
            start: insert_pos,
            end: insert_pos,
            text: injection,
        });

        branch_entry.branches.insert(
            current_id.to_string(),
            BranchLocation {
                kind: "stmt".to_string(),
                line,
                col,
                source: line_text,
            },
        );
    }

    Ok(ops)
}

fn instrument_function(func: &DbFunction) -> Result<(String, BranchMapEntry, Vec<Op>)> {
    let parts = split_function_def(&func.definition)?;
    let body = parts.body.clone();

    let mut branch_id: u32 = 1;
    let mut branch_entry = BranchMapEntry {
        schema: func.schema.clone(),
        name: func.name.clone(),
        args: func.args.clone(),
        branches: BTreeMap::new(),
    };

    let mut ops: Vec<Op> = Vec::new();

    // Layer 0: Statement/line coverage (PL/pgSQL statements)
    let stmt_ops = find_statement_lines(&body, func.oid, &mut branch_id, &mut branch_entry)?;
    ops.extend(stmt_ops);

    // Layer 1: PL/pgSQL control flow (simple regex-based extraction)
    let plpgsql_ops = find_plpgsql_branches(&body, func.oid, &mut branch_id, &mut branch_entry)?;
    ops.extend(plpgsql_ops);

    // Layer 2: SQL CASE WHEN (use pg_query.rs for parsing validation, regex for offsets)
    validate_sql_fragments(&body);
    let sql_ops = find_case_when_branches(&body, func.oid, &mut branch_id, &mut branch_entry)?;
    ops.extend(sql_ops);

    // Layer 3: SQL expressions (COALESCE / NULLIF)
    let coalesce_ops =
        find_coalesce_branches(&body, func.oid, &mut branch_id, &mut branch_entry, &ops)?;
    ops.extend(coalesce_ops);
    let nullif_ops =
        find_nullif_branches(&body, func.oid, &mut branch_id, &mut branch_entry, &ops)?;
    ops.extend(nullif_ops);

    let instrumented_body = apply_ops(&body, &ops)?;
    let instrumented = format!("{}{}{}", parts.prefix, instrumented_body, parts.suffix);

    Ok((instrumented, branch_entry, ops))
}

fn split_function_def(definition: &str) -> Result<BodyParts> {
    let re = Regex::new(r"(?i)\bAS\b").unwrap();
    let as_match = re
        .find(definition)
        .ok_or_else(|| anyhow!("Unable to locate AS keyword"))?;

    let after_as = &definition[as_match.end()..];
    let start_tag = after_as
        .find('$')
        .ok_or_else(|| anyhow!("Unable to locate function body delimiter"))?;
    let tag_start = as_match.end() + start_tag;

    let tag_end = definition[tag_start + 1..]
        .find('$')
        .ok_or_else(|| anyhow!("Invalid dollar-quote tag"))?
        + tag_start
        + 1;
    let tag = &definition[tag_start + 1..tag_end];
    let end_tag = format!("${tag}$");

    let body_start = tag_end + 1;
    let body_end = definition[body_start..]
        .find(&end_tag)
        .ok_or_else(|| anyhow!("Unable to locate end tag"))?
        + body_start;

    Ok(BodyParts {
        prefix: definition[..body_start].to_string(),
        body: definition[body_start..body_end].to_string(),
        suffix: definition[body_end..].to_string(),
    })
}

fn find_plpgsql_branches(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();
    let mut if_stack: Vec<()> = Vec::new();
    let mut exception_stack: Vec<usize> = Vec::new();
    let mut begin_depth = 0usize;
    let mut case_depth = 0usize;
    let mut expr_case_depth = 0usize;
    let mut skip_next_token = false;

    for (idx, token) in tokens.iter().enumerate() {
        let upper = token.upper.as_str();

        if skip_next_token {
            skip_next_token = false;
            continue;
        }

        if is_in_ranges(token.start, &sql_ranges) {
            continue;
        }

        if upper == "BEGIN" && is_stmt_start(body, &mask, &tokens, idx) {
            begin_depth += 1;
            continue;
        }

        if upper == "CASE" {
            if is_stmt_start(body, &mask, &tokens, idx) {
                case_depth += 1;
            } else {
                expr_case_depth += 1;
            }
            continue;
        }

        if upper == "IF" && is_stmt_start(body, &mask, &tokens, idx) && expr_case_depth == 0 {
            let current_id = *branch_id;
            *branch_id += 1;
            if let Some(then_idx) = find_then_token(&tokens, body, &mask, idx) {
                inject_plpgsql_branch(BranchInject {
                    body,
                    insert_pos: tokens[then_idx].start,
                    source_pos: token.start,
                    oid,
                    branch_id: current_id,
                    kind: "if",
                    ops: &mut ops,
                    branch_entry,
                });
            }
            if_stack.push(());
            continue;
        }

        if upper == "ELSIF"
            && is_stmt_start(body, &mask, &tokens, idx)
            && if_stack.last().is_some()
            && expr_case_depth == 0
        {
            let current_id = *branch_id;
            *branch_id += 1;
            if let Some(then_idx) = find_then_token(&tokens, body, &mask, idx) {
                inject_plpgsql_branch(BranchInject {
                    body,
                    insert_pos: tokens[then_idx].start,
                    source_pos: token.start,
                    oid,
                    branch_id: current_id,
                    kind: "elsif",
                    ops: &mut ops,
                    branch_entry,
                });
            }
            continue;
        }

        if upper == "ELSE"
            && is_stmt_start(body, &mask, &tokens, idx)
            && if_stack.last().is_some()
            && expr_case_depth == 0
        {
            let current_id = *branch_id;
            *branch_id += 1;
            inject_plpgsql_branch(BranchInject {
                body,
                insert_pos: token.start,
                source_pos: token.start,
                oid,
                branch_id: current_id,
                kind: "else",
                ops: &mut ops,
                branch_entry,
            });
            continue;
        }

        if upper == "EXCEPTION" && is_stmt_start(body, &mask, &tokens, idx) && expr_case_depth == 0
        {
            exception_stack.push(begin_depth);
            continue;
        }

        if upper == "WHEN" && is_stmt_start(body, &mask, &tokens, idx) {
            if !exception_stack.is_empty() && case_depth == 0 && expr_case_depth == 0 {
                let current_id = *branch_id;
                *branch_id += 1;
                if let Some(then_idx) = find_then_token(&tokens, body, &mask, idx) {
                    inject_plpgsql_branch(BranchInject {
                        body,
                        insert_pos: tokens[then_idx].start,
                        source_pos: token.start,
                        oid,
                        branch_id: current_id,
                        kind: "exception",
                        ops: &mut ops,
                        branch_entry,
                    });
                }
            }
            continue;
        }

        if upper == "END" {
            let mut handled_block = false;
            if let Some(next) = tokens.get(idx + 1) {
                match next.upper.as_str() {
                    "IF" => {
                        if_stack.pop();
                        skip_next_token = true;
                        handled_block = true;
                    }
                    "CASE" => {
                        case_depth = case_depth.saturating_sub(1);
                        skip_next_token = true;
                        handled_block = true;
                    }
                    "BEGIN" => {
                        begin_depth = begin_depth.saturating_sub(1);
                        skip_next_token = true;
                        handled_block = true;
                        while let Some(depth) = exception_stack.last().copied() {
                            if depth > begin_depth {
                                exception_stack.pop();
                            } else {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !handled_block && expr_case_depth > 0 {
                expr_case_depth = expr_case_depth.saturating_sub(1);
            }
        }
    }

    let mut case_ops = find_plpgsql_case_branches(body, oid, branch_id, branch_entry)?;
    ops.append(&mut case_ops);

    Ok(ops)
}

fn find_then_token(tokens: &[Token], body: &str, mask: &[bool], start_idx: usize) -> Option<usize> {
    let mut case_depth = 0usize;
    let mut skip_next_token = false;
    for idx in (start_idx + 1)..tokens.len() {
        let upper = tokens[idx].upper.as_str();
        if skip_next_token {
            skip_next_token = false;
            continue;
        }

        if upper == "CASE" && !is_stmt_start(body, mask, tokens, idx) {
            case_depth += 1;
            continue;
        }

        if upper == "END" && case_depth > 0 {
            if tokens.get(idx + 1).map(|t| t.upper.as_str()) == Some("CASE") {
                case_depth = case_depth.saturating_sub(1);
                skip_next_token = true;
            }
            continue;
        }

        if upper == "THEN" && case_depth == 0 {
            return Some(idx);
        }

        if matches!(upper, "IF" | "ELSIF" | "ELSE" | "EXCEPTION")
            && is_stmt_start(body, mask, tokens, idx)
        {
            break;
        }
    }
    None
}

/// Build a coverage-tracking call that preserves PL/pgSQL `FOUND` and `ROW_COUNT`.
///
/// A bare `PERFORM rpcc.track_*(...)` sets both `FOUND` and the `GET DIAGNOSTICS
/// ROW_COUNT` value, which corrupts any following `IF NOT FOUND` / `GET DIAGNOSTICS`
/// in the instrumented function — changing its behavior and making those branches
/// unreachable under coverage. An *assignment* statement does not touch `FOUND` or
/// `ROW_COUNT`, so we call the tracker via an assignment inside a self-contained
/// nested block (its own DECLARE means no variable has to be added to the function).
fn track_call(func: &str, oid: u32, branch_id: u32) -> String {
    format!("DECLARE rpcc_hit boolean; BEGIN rpcc_hit := {func}({oid}, {branch_id}); END;")
}

fn inject_plpgsql_branch(ctx: BranchInject<'_>) {
    let (insert_line_start, insert_line_end) = line_bounds(ctx.body, ctx.insert_pos);
    let insert_line = &ctx.body[insert_line_start..insert_line_end];
    let indent: String = insert_line
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let injection = format!(
        "{indent}  {}\n",
        track_call("rpcc.track", ctx.oid, ctx.branch_id)
    );

    ctx.ops.push(Op {
        kind: OpKind::Insert,
        start: insert_line_end,
        end: insert_line_end,
        text: injection,
    });

    let (source_line_start, source_line_end) = line_bounds(ctx.body, ctx.source_pos);
    let source_line = &ctx.body[source_line_start..source_line_end];
    let (line, col) = line_col_from_offset(ctx.body, ctx.source_pos);
    let source = source_line.trim_end().to_string();

    ctx.branch_entry.branches.insert(
        ctx.branch_id.to_string(),
        BranchLocation {
            kind: ctx.kind.to_string(),
            line,
            col,
            source,
        },
    );
}

fn find_plpgsql_case_branches(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();
    let mut block_stack: Vec<&'static str> = Vec::new();
    let mut skip_next_token = false;

    for (idx, token) in tokens.iter().enumerate() {
        let upper = token.upper.as_str();

        if skip_next_token {
            skip_next_token = false;
            continue;
        }

        if is_in_ranges(token.start, &sql_ranges) {
            continue;
        }

        if upper == "CASE" && is_stmt_start(body, &mask, &tokens, idx) {
            let stmt_end = next_unmasked_semicolon(body, &mask, token.start).unwrap_or(body.len());
            if !has_end_case_before(body, &mask, &tokens, idx, stmt_end) {
                continue;
            }
            block_stack.push("CASE");
            continue;
        }

        if upper == "IF" && is_stmt_start(body, &mask, &tokens, idx) {
            block_stack.push("IF");
            continue;
        }

        if upper == "LOOP" && is_stmt_start(body, &mask, &tokens, idx) {
            block_stack.push("LOOP");
            continue;
        }

        if upper == "BEGIN" && is_stmt_start(body, &mask, &tokens, idx) {
            block_stack.push("BEGIN");
            continue;
        }

        if upper == "END" {
            let next = tokens.get(idx + 1);
            if let Some(next) = next {
                let kind = match next.upper.as_str() {
                    "CASE" => Some("CASE"),
                    "IF" => Some("IF"),
                    "LOOP" => Some("LOOP"),
                    "BEGIN" => Some("BEGIN"),
                    _ => None,
                };
                if let Some(kind) = kind {
                    if block_stack.last() == Some(&kind) {
                        block_stack.pop();
                    }
                    skip_next_token = true;
                }
            }
            continue;
        }

        if upper == "WHEN" || upper == "ELSE" {
            if block_stack.last() != Some(&"CASE") {
                continue;
            }
            let current_id = *branch_id;
            *branch_id += 1;

            let (line_start, line_end) = line_bounds(body, token.start);
            let line_text = &body[line_start..line_end];
            let indent: String = line_text
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let insert_at = line_end;

            let injection = format!("{indent}  {}\n", track_call("rpcc.track", oid, current_id));

            ops.push(Op {
                kind: OpKind::Insert,
                start: insert_at,
                end: insert_at,
                text: injection,
            });

            let (line, col) = line_col_from_offset(body, token.start);
            let source = line_text.trim_end().to_string();
            let kind = if upper == "WHEN" {
                "plpgsql_case_when"
            } else {
                "plpgsql_case_else"
            };

            branch_entry.branches.insert(
                current_id.to_string(),
                BranchLocation {
                    kind: kind.to_string(),
                    line,
                    col,
                    source,
                },
            );
        }
    }

    Ok(ops)
}

fn find_case_when_branches(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();

    #[derive(Debug)]
    struct CaseCtx {
        searched: bool,
        pending_when_start: Option<usize>,
        when_token_start: Option<usize>,
        is_sql: bool,
    }

    let mut case_stack: Vec<CaseCtx> = Vec::new();
    let mut skip_next_case = false;

    for (idx, token) in tokens.iter().enumerate() {
        let upper = token.upper.as_str();

        if skip_next_case {
            if upper == "CASE" {
                skip_next_case = false;
                continue;
            }
            skip_next_case = false;
        }

        if upper == "CASE" {
            let is_plpgsql_stmt = is_stmt_start(body, &mask, &tokens, idx);
            let in_sql = is_in_ranges(token.start, &sql_ranges);
            let is_sql = !is_plpgsql_stmt && in_sql;
            let searched = tokens
                .get(idx + 1)
                .map(|t| t.upper.as_str() == "WHEN")
                .unwrap_or(false);
            case_stack.push(CaseCtx {
                searched,
                pending_when_start: None,
                when_token_start: None,
                is_sql,
            });
            continue;
        }

        if upper == "END" {
            if let Some(next) = tokens.get(idx + 1) {
                if next.upper.as_str() == "CASE" {
                    case_stack.pop();
                    skip_next_case = true;
                }
            }
            continue;
        }

        if upper == "WHEN" {
            if let Some(ctx) = case_stack.last_mut() {
                if ctx.is_sql && ctx.searched {
                    ctx.pending_when_start = Some(token.end);
                    ctx.when_token_start = Some(token.start);
                }
            }
            continue;
        }

        if upper == "THEN" {
            if let Some(ctx) = case_stack.last_mut() {
                if let Some(start) = ctx.pending_when_start.take() {
                    if let Some(when_start) = ctx.when_token_start.take() {
                        if !ctx.is_sql {
                            continue;
                        }
                        let end = token.start;
                        if start <= end && end <= body.len() {
                            let cond = &body[start..end];
                            let current_id = *branch_id;
                            *branch_id += 1;

                            let cond_trimmed = cond.trim_end();
                            let whitespace = &cond[cond_trimmed.len()..];
                            let replacement = format!(
                                "WHEN rpcc.track_bool({oid}, {current_id}, {cond_trimmed}){whitespace}THEN"
                            );

                            let (line, col) = line_col_from_offset(body, when_start);
                            let source = body[when_start..end].trim_end().to_string();

                            ops.push(Op {
                                kind: OpKind::Replace,
                                start: when_start,
                                end: token.end,
                                text: replacement,
                            });

                            branch_entry.branches.insert(
                                current_id.to_string(),
                                BranchLocation {
                                    kind: "case_when".to_string(),
                                    line,
                                    col,
                                    source,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(ops)
}

fn next_non_ws_unmasked(body: &str, mask: &[bool], start: usize) -> Option<(usize, u8)> {
    let bytes = body.as_bytes();
    for (idx, byte) in bytes.iter().enumerate().skip(start) {
        if mask.get(idx).copied().unwrap_or(false) {
            continue;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        return Some((idx, *byte));
    }
    None
}

fn find_matching_paren(body: &str, mask: &[bool], open_pos: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    for (idx, byte) in bytes.iter().enumerate().skip(open_pos) {
        if mask.get(idx).copied().unwrap_or(false) {
            continue;
        }
        match *byte {
            b'(' => {
                depth += 1;
            }
            b')' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_call_parens(body: &str, mask: &[bool], start: usize) -> Option<(usize, usize)> {
    let (open_pos, ch) = next_non_ws_unmasked(body, mask, start)?;
    if ch != b'(' {
        return None;
    }
    let close_pos = find_matching_paren(body, mask, open_pos)?;
    Some((open_pos, close_pos))
}

fn split_args(body: &str, mask: &[bool], start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut arg_start = start;
    let mut i = start;
    while i < end {
        if mask.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
            }
            b',' if depth == 0 => {
                args.push((arg_start, i));
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if arg_start <= end {
        args.push((arg_start, end));
    }
    args
}

fn trim_span(body: &str, mut start: usize, mut end: usize) -> Option<(usize, usize)> {
    let bytes = body.as_bytes();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if start >= end {
        None
    } else {
        Some((start, end))
    }
}

fn overlaps_replace(start: usize, end: usize, ops: &[Op]) -> bool {
    ops.iter()
        .any(|op| matches!(op.kind, OpKind::Replace) && start < op.end && end > op.start)
}

fn is_untyped_string_literal(body: &str, start: usize, end: usize) -> bool {
    let slice = body[start..end].trim();
    let bytes = slice.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let last = bytes[bytes.len() - 1];
    if last != b'\'' {
        return false;
    }
    match bytes[0] {
        b'\'' => true,
        b'E' | b'e' | b'B' | b'b' | b'X' | b'x' => bytes.get(1) == Some(&b'\''),
        b'U' | b'u' => bytes.get(1) == Some(&b'&') && bytes.get(2) == Some(&b'\''),
        _ => false,
    }
}

fn is_untyped_null_literal(body: &str, start: usize, end: usize) -> bool {
    body[start..end].trim().eq_ignore_ascii_case("NULL")
}

fn find_coalesce_branches(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
    existing_ops: &[Op],
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();

    for token in tokens {
        if token.upper.as_str() != "COALESCE" {
            continue;
        }
        if !is_in_ranges(token.start, &sql_ranges) {
            continue;
        }
        let Some((open_pos, close_pos)) = find_call_parens(body, &mask, token.end) else {
            continue;
        };
        if !is_in_ranges(open_pos, &sql_ranges) {
            continue;
        }
        let args = split_args(body, &mask, open_pos + 1, close_pos);
        for (arg_start, arg_end) in args {
            if let Some((start, end)) = trim_span(body, arg_start, arg_end) {
                if overlaps_replace(start, end, existing_ops) {
                    continue;
                }
                // Avoid polymorphic type errors from wrapping bare string/NULL literals.
                if is_untyped_string_literal(body, start, end)
                    || is_untyped_null_literal(body, start, end)
                {
                    continue;
                }
                let current_id = *branch_id;
                *branch_id += 1;
                let (line, col) = line_col_from_offset(body, start);
                let source = body[start..end].trim_end().to_string();
                ops.push(Op {
                    kind: OpKind::Insert,
                    start,
                    end: start,
                    text: format!("rpcc.track_any({oid}, {current_id}, "),
                });
                ops.push(Op {
                    kind: OpKind::Insert,
                    start: end,
                    end,
                    text: ")".to_string(),
                });
                branch_entry.branches.insert(
                    current_id.to_string(),
                    BranchLocation {
                        kind: "coalesce_arg".to_string(),
                        line,
                        col,
                        source,
                    },
                );
            }
        }
    }

    Ok(ops)
}

fn find_nullif_branches(
    body: &str,
    oid: u32,
    branch_id: &mut u32,
    branch_entry: &mut BranchMapEntry,
    existing_ops: &[Op],
) -> Result<Vec<Op>> {
    let mask = code_mask(body);
    let tokens = tokenize(body, &mask);
    let sql_ranges = sql_statement_ranges(body, &mask, &tokens);
    let mut ops = Vec::new();

    for token in tokens {
        if token.upper.as_str() != "NULLIF" {
            continue;
        }
        if !is_in_ranges(token.start, &sql_ranges) {
            continue;
        }
        let Some((open_pos, close_pos)) = find_call_parens(body, &mask, token.end) else {
            continue;
        };
        if !is_in_ranges(open_pos, &sql_ranges) {
            continue;
        }
        let args = split_args(body, &mask, open_pos + 1, close_pos);
        if args.len() != 2 {
            continue;
        }
        for (arg_start, arg_end) in args {
            if let Some((start, end)) = trim_span(body, arg_start, arg_end) {
                if overlaps_replace(start, end, existing_ops) {
                    continue;
                }
                // Avoid polymorphic type errors from wrapping bare string/NULL literals.
                if is_untyped_string_literal(body, start, end)
                    || is_untyped_null_literal(body, start, end)
                {
                    continue;
                }
                let current_id = *branch_id;
                *branch_id += 1;
                let (line, col) = line_col_from_offset(body, start);
                let source = body[start..end].trim_end().to_string();
                ops.push(Op {
                    kind: OpKind::Insert,
                    start,
                    end: start,
                    text: format!("rpcc.track_any({oid}, {current_id}, "),
                });
                ops.push(Op {
                    kind: OpKind::Insert,
                    start: end,
                    end,
                    text: ")".to_string(),
                });
                branch_entry.branches.insert(
                    current_id.to_string(),
                    BranchLocation {
                        kind: "nullif_arg".to_string(),
                        line,
                        col,
                        source,
                    },
                );
            }
        }
    }

    Ok(ops)
}

fn validate_sql_fragments(_body: &str) {
    // Phase 0: pg_query integration omitted (crate unavailable offline).
    // Placeholder for SQL parsing validation once pg_query is available.
}

fn apply_ops(body: &str, ops: &[Op]) -> Result<String> {
    let mut bytes = body.as_bytes().to_vec();

    let mut sorted_ops = ops.to_vec();
    sorted_ops.sort_by(|a, b| b.start.cmp(&a.start));

    for op in sorted_ops {
        if op.start > bytes.len() || op.end > bytes.len() || op.start > op.end {
            return Err(anyhow!("Invalid op range {}..{}", op.start, op.end));
        }
        if !body.is_char_boundary(op.start) || !body.is_char_boundary(op.end) {
            return Err(anyhow!("Injection point not on UTF-8 boundary"));
        }
        bytes.splice(op.start..op.end, op.text.as_bytes().iter().copied());
    }

    String::from_utf8(bytes).context("Invalid UTF-8 after injection")
}

fn line_col_from_offset(body: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (idx, ch) in body.char_indices() {
        if idx == offset {
            return (line, col);
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_call_uses_assignment_not_perform() {
        // A bare PERFORM would set FOUND/ROW_COUNT and corrupt the instrumented
        // function's control flow (e.g. break IF NOT FOUND). The tracker must be
        // invoked via an assignment, which leaves FOUND/ROW_COUNT untouched.
        let call = track_call("rpcc.track_line", 42, 7);
        assert!(
            !call.contains("PERFORM"),
            "track call must not use PERFORM (it clobbers FOUND/ROW_COUNT): {call}"
        );
        assert!(
            call.contains(":= rpcc.track_line(42, 7)"),
            "track call must invoke the tracker via assignment: {call}"
        );
        // Self-contained nested block so no DECLARE is needed in the target function.
        assert!(call.contains("DECLARE") && call.contains("BEGIN") && call.contains("END;"));
    }

    #[test]
    fn instrumented_if_not_found_is_preceded_by_assignment_form() {
        // End-to-end at the string level: instrumenting a function whose body has a
        // SELECT INTO followed by IF NOT FOUND must inject the assignment form (not a
        // PERFORM) on the line before IF NOT FOUND.
        let func = DbFunction {
            oid: 1,
            schema: "public".to_string(),
            name: "f".to_string(),
            args: String::new(),
            xmin: "1".to_string(),
            definition: concat!(
                "CREATE FUNCTION public.f() RETURNS void LANGUAGE plpgsql AS $$\n",
                "DECLARE r record;\n",
                "BEGIN\n",
                "  SELECT 1 INTO r WHERE false;\n",
                "  IF NOT FOUND THEN\n",
                "    RAISE EXCEPTION 'missing';\n",
                "  END IF;\n",
                "END;\n",
                "$$;"
            )
            .to_string(),
        };
        let (instrumented, _entry, _ops) = instrument_function(&func).unwrap();
        assert!(
            !instrumented.contains("PERFORM rpcc."),
            "no bare PERFORM tracker calls should be emitted:\n{instrumented}"
        );
        // The line immediately before `IF NOT FOUND` must be the assignment form.
        let idx = instrumented.find("IF NOT FOUND").expect("IF NOT FOUND present");
        let before = &instrumented[..idx];
        assert!(
            before.trim_end().ends_with("END;"),
            "IF NOT FOUND must be preceded by the nested-block assignment tracker:\n{instrumented}"
        );
    }
}
