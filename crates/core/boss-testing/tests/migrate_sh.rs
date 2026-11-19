//! Behavior of `infra/postgres/migrate.sh` — the only path schema takes
//! into a database.
//!
//! The runner's contract, pinned here:
//! - schema/*.sql sorted by NNN- prefix is the migration order; files not yet recorded
//!   in `schema_migrations` apply in order, each atomically with its
//!   record, so a re-run never re-applies and a failure records nothing.
//! - `--baseline` records without running, for databases that predate the
//!   runner (their tables already exist).
//! - `--without <name>` skips matching entries without recording them —
//!   the ledger-less bootstrap capability (`TestDb::new_without` mirrors it).
//! - An already-applied file whose content has changed fails the run by
//!   name: applied migrations are history, changes go in a new file.
//!
//! Each test creates its own scratch database (dropped at the end; a
//! panic can leak one — it is named through `scratch_database_name`, so
//! TestDb's orphan sweep finds it by the same stamp and leaves it alone
//! until then). The synthetic-schema tests copy the
//! script into a temp dir beside a tiny schema/ so they can grow, break,
//! and edit migrations without touching the repo's real schema files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::str::FromStr;

use boss_testing::test_db::{release_scratch_database, scratch_database_name};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://boss:boss@127.0.0.1/postgres";

#[test]
fn post_start_deadlines_kill_and_reap_the_whole_owned_group() {
    let code = r#"
import importlib.util, pathlib, sys, tempfile, os
path = pathlib.Path(sys.argv[1])
assert path.is_file(), 'post-start execution has no process-group deadline owner'
spec = importlib.util.spec_from_file_location('post_start_bound', path)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
with tempfile.TemporaryDirectory() as d:
    marker = pathlib.Path(d) / 'pids'
    fixture = pathlib.Path(d) / 'fixture.py'
    fixture.write_text('import os, signal, time, sys\n'
                       'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                       'child=os.fork()\n'
                       'if child == 0:\n'
                       ' signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                       ' while True: time.sleep(10)\n'
                       'open(sys.argv[1], "w").write(str(os.getpid())+" "+str(child))\n'
                       'while True: time.sleep(10)\n')
    rc = m.run_owned([sys.executable, str(fixture), str(marker)], 1, 0.1, 1)
    assert rc == 124, ('deadline must remain failure', rc)
    assert marker.is_file(), 'actual child and descendant must have started'
    for pid in map(int, marker.read_text().split()):
        try: os.kill(pid, 0)
        except ProcessLookupError: pass
        else: raise AssertionError(('owned process remains, including zombie', pid))
    try: os.waitpid(-1, os.WNOHANG)
    except ChildProcessError: pass
    else: raise AssertionError('owned children were not reaped')
    assert m.run_owned([sys.executable, '-c', 'raise SystemExit(42)'], 1, 0.1, 1) == 42
    assert m.run_owned([sys.executable, '-c', 'pass'], 1, 0.1, 1) == 0
assert m.MODES == {'--pending': 15, '--apply': 600}
manifest = pathlib.Path(sys.argv[2]).read_text()
assert 'activeDeadlineSeconds: 1200' in manifest
assert 'name: BOSS_ACT_BOUND_S, value: "606"' in manifest
assert 'post-start-bound.py --pending' in manifest
assert 'post-start-bound.py --apply' in manifest
"#;
    let root = real_script().parent().unwrap().to_path_buf();
    let out = std::process::Command::new("python3")
        .args(["-B", "-c", code])
        .arg(root.join("post-start-bound.py"))
        .arg(root.join("../cluster/manifests/boss-post-start-schema.yaml"))
        .output()
        .unwrap();
    assert_ok(&out, "owned post-start process-group deadline");
}

#[test]
fn post_start_retains_the_initial_refusal_beside_the_actual_recovery_outcome() {
    let code = r#"
import pathlib, subprocess, sys, tempfile
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    helper = d / 'post-start-bound.py'
    helper.write_bytes(pathlib.Path(sys.argv[1]).read_bytes())
    observation = b'initial refusal: deadline\r\nfull retained diagnostic\n'
    capture = d / 'precheck'
    capture.write_bytes(observation)
    for outcome in [0, 42]:
        (d / 'post-start.sh').write_text('echo actual-apply-output; exit '+str(outcome)+'\n')
        out = subprocess.run(['bash', '-c', 'exec 3< "$1"; exec python3 -B "$2" --apply-after-precheck 124', 'fixture', str(capture), str(helper)], capture_output=True)
        assert out.returncode == outcome, (out.returncode, out.stdout, out.stderr)
        assert observation in out.stdout, 'initial complete bytes were discarded'
        assert b'initial precheck exit 124' in out.stdout
        assert b'actual-apply-output' in out.stdout
        assert ('apply outcome exit '+str(outcome)).encode() in out.stdout
"#;
    let out = std::process::Command::new("python3")
        .args(["-B", "-c", code])
        .arg(real_script().with_file_name("post-start-bound.py"))
        .output()
        .unwrap();
    assert_ok(&out, "durable precheck and recovery phase conservation");
}

/// f93bdee4: every post-start pass from 2026-11-01 to 2026-11-09 (11,299
/// packets, one a minute) ended "command left descendants; refusing
/// success" (exit 125) while migrate.sh had printed that no entry was
/// pending. The descendant was migrate.sh's own advisory-lock session:
/// `coproc MIGLOCK { psql; }` holds the lock for the whole run and nothing
/// ended it, so it was still connected — or an orphan nobody had reaped —
/// when its leader exited, and the bound wrapper, which owns every
/// descendant until reaped, refused correctly. So the real runner runs here
/// under the real wrapper, with a stand-in psql whose lock session takes a
/// moment to close after EOF, as a real connection does. Repeated, because
/// a descendant that is only sometimes left is still a failed pass a minute.
/// No database: the session leaks before the runner reads anything.
#[test]
fn a_post_start_pass_ends_its_lock_session_before_it_exits() {
    let code = r##"
import hashlib, importlib.util, os, pathlib, shutil, sys, tempfile
spec = importlib.util.spec_from_file_location('post_start_bound', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
FAKE = r'''#!/bin/bash
# With -c: the runner's bookkeeping. Without: the lock session, one line
# per statement until EOF, then a moment to close the connection.
for a in "$@"; do
    if [ "$a" = -c ]; then
        case "${@: -1}" in
            *checksum\ FROM\ schema_migrations*) printf '%s\n' "$FAKE_LEDGER" ;;
            *to_regclass*) echo f ;;
        esac
        exit 0
    fi
done
while IFS= read -r _; do echo t; done
sleep 0.2
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    shutil.copy(sys.argv[2], d / 'migrate.sh')
    (d / 'schema').mkdir()
    boot = b'CREATE TABLE t (id int);\n'
    (d / 'schema' / '001-boot.sql').write_bytes(boot)
    psql = d / 'psql'
    psql.write_text(FAKE)
    psql.chmod(0o755)
    os.environ['FAKE_LEDGER'] = '001-boot.sql|' + hashlib.sha256(boot).hexdigest()
    argv = ['/bin/bash', str(d / 'migrate.sh'), '--phase', 'post-start']
    for i in range(5):
        for mode in (['--pending'], []):
            rc = m.run_owned(argv + mode + ['--', str(psql)], 15, 0.5, 1)
            assert rc == 0, ('a paid post-start pass must succeed under its owner', mode, i, rc)
    # A refusal keeps its own status: owed debt is 75, never 125.
    index = b'-- migrate: no-transaction\nCREATE INDEX CONCURRENTLY i ON t (id);\n'
    (d / 'schema' / '002-index.sql').write_bytes(index)
    (d / 'migration-phases').mkdir()
    (d / 'migration-phases' / '002-index.sql.phase').write_text(
        'post-start ' + hashlib.sha256(index).hexdigest() + '\n')
    rc = m.run_owned(argv + ['--pending', '--', str(psql)], 15, 0.5, 1)
    assert rc == 75, ('owed debt must answer 75', rc)
"##;
    let out = std::process::Command::new("python3")
        .args(["-B", "-c", code])
        .arg(real_script().with_file_name("post-start-bound.py"))
        .arg(real_script())
        .output()
        .unwrap();
    assert_ok(&out, "a post-start pass under its bound owner");
}

fn admin_url() -> String {
    std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string())
}

/// Swap the database name in a `postgres://...` URL. Assumes the URL's
/// last path segment is the database — true of the plain URLs this
/// harness runs with (see `DEFAULT_ADMIN_URL`, CI's service URL).
fn with_db(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let cut = base.rfind('/').expect("admin url has no path segment");
    let mut out = format!("{}/{db}", &base[..cut]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

fn real_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/postgres/migrate.sh")
}

fn real_schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/postgres/schema")
}

async fn admin_conn() -> PgConnection {
    let opts = PgConnectOptions::from_str(&admin_url()).expect("parsing admin url");
    PgConnection::connect_with(&opts)
        .await
        .expect("connecting to admin db")
}

/// Create an empty scratch database and return `(name, url)`.
///
/// THE RACE THIS HAD (2026-09-19, backlog 2a056500). The name was
/// `test_boss_mig_<hex>` — TestDb's prefix with no stamp, which its
/// orphan sweep reads as ancient litter and drops the moment no session
/// is connected. Every `TestDb::new` in every other test process runs
/// that sweep, and between `CREATE DATABASE` here and migrate.sh's
/// first connection nothing is connected — so under the full
/// `--all-features` run `editing_an_applied_migration_fails_the_next_
/// run_by_name` redded 1-in-N with `FATAL: database "test_boss_mig_
/// 09977214ad4e" does not exist — It seems to have just been dropped or
/// renamed`, and passed alone, where nothing sweeps. A stamped name
/// lives its TTL like TestDb's own.
async fn scratch_db() -> (String, String) {
    let name = scratch_database_name("mig");
    let mut admin = admin_conn().await;
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .expect("CREATE DATABASE");
    let url = with_db(&admin_url(), &name);
    (name, url)
}

/// Through TestDb's reaper, not a `DROP DATABASE` of our own: every
/// drop forces a checkpoint of the whole server, and one issued here
/// held this test until it finished — on the dev pod on 2026-09-23 a
/// checkpoint took about 40 s, and this binary is where three builders'
/// local suites were seen to stop (backlog 9e1ea321).
fn drop_db(name: &str) {
    release_scratch_database(&admin_url(), name);
}

/// Run a migrate.sh (real or copied) against `db_url` with the given
/// script args, connecting via `-- psql <url>`.
///
/// Spawned via `bash <script>`, never by exec'ing the copy directly:
/// tests run as threads of one process, and a fork in thread A can
/// inherit thread B's still-open write-fd from `fs::copy` — exec'ing
/// B's file then fails ETXTBSY ("Text file busy", first struck on
/// train #222, `df69249e`). bash opens the script READ-ONLY, so the
/// race is impossible by construction.
fn run(script: &Path, args: &[&str], db_url: &str) -> Output {
    std::process::Command::new("bash")
        .arg(script)
        .args(args)
        .args(["--", "psql", db_url])
        .output()
        .unwrap_or_else(|e| panic!("spawning {}: {e}", script.display()))
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// A temp copy of the runner beside a synthetic schema/ dir the test can
/// mutate freely. `files` are `(filename, sql)`; the runner derives the
/// order from the `NNN-` prefixes, so there is no list to write.
struct Synthetic {
    dir: PathBuf,
}

impl Synthetic {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("boss-migrate-test-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(dir.join("schema")).expect("mkdir schema");
        // fs::copy preserves the exec bit on unix.
        // mode-bits-ok: run only as bash <path> (see run), opened read-only
        fs::copy(real_script(), dir.join("migrate.sh")).expect("copying migrate.sh");
        let s = Self { dir };
        for (name, sql) in files {
            s.write(name, sql);
        }
        s
    }

    fn script(&self) -> PathBuf {
        self.dir.join("migrate.sh")
    }

    fn write(&self, name: &str, sql: &str) {
        fs::write(self.dir.join("schema").join(name), sql).expect("writing schema file");
    }

    fn phase(&self, name: &str, phase: &str) {
        let directory = self.dir.join("migration-phases");
        fs::create_dir_all(&directory).unwrap();
        let hash = std::process::Command::new("sha256sum")
            .arg(self.dir.join("schema").join(name))
            .output()
            .unwrap();
        assert!(hash.status.success());
        let hash = String::from_utf8(hash.stdout).unwrap();
        let hash = hash.split_whitespace().next().unwrap();
        fs::write(
            directory.join(format!("{name}.phase")),
            format!("{phase} {hash}\n"),
        )
        .unwrap();
    }
}

impl Drop for Synthetic {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn connect(db_url: &str) -> PgConnection {
    let opts = PgConnectOptions::from_str(db_url).expect("parsing db url");
    PgConnection::connect_with(&opts)
        .await
        .expect("connecting to scratch db")
}

/// Recorded migration ids in application order.
async fn recorded(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query("SELECT id FROM schema_migrations ORDER BY applied_at, id")
        .fetch_all(conn)
        .await
        .expect("reading schema_migrations")
        .into_iter()
        .map(|r| r.get::<String, _>("id"))
        .collect()
}

async fn recorded_row(conn: &mut PgConnection, id: &str) -> (String, String) {
    let row = sqlx::query(
        "SELECT checksum, applied_at::text AS applied_at FROM schema_migrations WHERE id = $1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .unwrap_or_else(|e| panic!("no schema_migrations row for {id}: {e}"));
    (row.get("checksum"), row.get("applied_at"))
}

async fn table_exists(conn: &mut PgConnection, table: &str) -> bool {
    sqlx::query("SELECT to_regclass($1) IS NOT NULL AS present")
        .bind(table)
        .fetch_one(conn)
        .await
        .expect("to_regclass")
        .get("present")
}

/// The migration order as the runner computes it: every `*.sql` in the
/// schema dir, sorted by its `NNN-` prefix. Mirrors migrate.sh's
/// `sort -t- -k1,1n` and build.rs's sort key — if those three ever
/// disagree, the tests below apply a different schema than production.
fn migration_order(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("reading the schema dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    names.sort_by_key(|n| {
        let num: u32 = n
            .split('-')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(u32::MAX);
        (num, n.clone())
    });
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_schema_applies_to_a_fresh_db_and_a_rerun_is_a_noop() {
    let (name, url) = scratch_db().await;

    let out = run(&real_script(), &[], &url);
    assert_ok(&out, "first run against a fresh db");

    let mut conn = connect(&url).await;
    let expected = migration_order(&real_schema_dir());
    let mut got = recorded(&mut conn).await;
    let mut want = expected.clone();
    got.sort();
    want.sort();
    assert_eq!(got, want, "every migration is recorded, nothing else");
    assert!(table_exists(&mut conn, "jobs").await, "03-jobs applied");
    assert!(
        table_exists(&mut conn, "audit_log").await,
        "02-events applied"
    );

    let before = recorded_row(&mut conn, &expected[0]).await;
    let out = run(&real_script(), &[], &url);
    assert_ok(&out, "second run against an up-to-date db");
    let after = recorded_row(&mut conn, &expected[0]).await;
    assert_eq!(before, after, "a re-run re-applies nothing");

    let upstream: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'stations' AND column_name = 'upstream')",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(
        !upstream,
        "the ordered schema and its rerun retire stations.upstream"
    );

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_testdb_schema_retires_the_station_upstream_column() {
    let db = boss_testing::TestDb::new().await;
    let upstream: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'public' AND table_name = 'stations' AND column_name = 'upstream')",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(
        !upstream,
        "TestDb applies historical station writers before the drop"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_unrecorded_entries_apply() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &[], &url), "applying 01-first");
    let mut conn = connect(&url).await;
    let first = recorded_row(&mut conn, "01-first.sql").await;

    // Dropping the file in IS the whole act of adding a migration —
    // there is no list to update, which is the point of the collapse.
    syn.write("02-second.sql", "CREATE TABLE t2 (id int);");
    assert_ok(
        &run(&syn.script(), &[], &url),
        "applying the newly-added migration",
    );

    assert!(table_exists(&mut conn, "t2").await, "02-second ran");
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-first.sql", "02-second.sql"],
        "both entries recorded, in order"
    );
    assert_eq!(
        first,
        recorded_row(&mut conn, "01-first.sql").await,
        "the already-applied entry was not touched"
    );

    drop(conn);
    drop_db(&name);
}

/// 6bc062b1: exercise the exact production migration and retry path.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_cut_index_migration_repairs_an_invalid_build_without_touching_other_indexes() {
    let migration = fs::read_dir(real_schema_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.to_string_lossy()
                .ends_with("-a-window-cut-order-check-is-covered.sql")
        })
        .expect("the window cut covering-index migration exists");
    let sql = fs::read_to_string(migration).unwrap();
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[(
        "01-table.sql",
        "CREATE TABLE audit_log (id bigint PRIMARY KEY, created_at timestamptz NOT NULL); CREATE INDEX unrelated_audit_index ON audit_log (created_at DESC);\n",
    )]);
    assert_ok(
        &run(&syn.script(), &[], &url),
        "creating the recorded fixture schema",
    );
    let mut conn = connect(&url).await;
    // A failed concurrent unique build leaves a real INVALID index.
    conn.execute("INSERT INTO audit_log VALUES (1,now()),(2,now())")
        .await
        .unwrap();
    assert!(
        conn.execute("CREATE UNIQUE INDEX CONCURRENTLY audit_log_created_at_id ON audit_log ((1))")
            .await
            .is_err()
    );
    let valid: bool = sqlx::query_scalar(
        "SELECT indisvalid FROM pg_index WHERE indexrelid='audit_log_created_at_id'::regclass",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(!valid);
    let unrelated: i64 =
        sqlx::query_scalar("SELECT 'unrelated_audit_index'::regclass::oid::bigint")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    syn.write("02-cover.sql", &sql);
    assert_ok(
        &run(&syn.script(), &[], &url),
        "repairing the invalid covering index",
    );
    let (definition, valid): (String, bool) = sqlx::query_as("SELECT pg_get_indexdef(indexrelid),indisvalid FROM pg_index WHERE indexrelid='audit_log_created_at_id'::regclass")
        .fetch_one(&mut conn).await.unwrap();
    assert!(
        valid && definition.ends_with("USING btree (created_at, id)"),
        "{definition}"
    );
    let unchanged: i64 =
        sqlx::query_scalar("SELECT 'unrelated_audit_index'::regclass::oid::bigint")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(unrelated, unchanged);
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-table.sql", "02-cover.sql"]
    );
    assert_ok(
        &run(&syn.script(), &[], &url),
        "replaying the recorded migration",
    );
    drop(conn);
    drop_db(&name);
}

/// Design c195e83e Q4 (2026-10-17): an index on a table that takes every
/// write is built CONCURRENTLY, and Postgres refuses that inside a
/// transaction block. Every other migration runs inside one with its
/// bookkeeping row, so a file opts out by its first line, runs alone,
/// and is recorded only after it succeeded. A failure records nothing,
/// so the next run tries the file again.
#[tokio::test(flavor = "multi_thread")]
async fn a_no_transaction_migration_runs_alone_and_is_recorded_after_it_succeeds() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[
        ("01-table.sql", "CREATE TABLE t1 (id int, doc jsonb);"),
        (
            "02-index.sql",
            "-- migrate: no-transaction\n\
             DROP INDEX CONCURRENTLY IF EXISTS t1_doc_id;\n\
             CREATE INDEX CONCURRENTLY t1_doc_id ON t1 ((doc->>'id'));\n",
        ),
        ("03-after.sql", "CREATE TABLE t3 (id int);"),
    ]);
    assert_ok(
        &run(&syn.script(), &[], &url),
        "applying a no-transaction file",
    );
    let mut conn = connect(&url).await;
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-table.sql", "02-index.sql", "03-after.sql"]
    );
    let valid: bool = sqlx::query_scalar(
        "SELECT i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid \
         WHERE c.relname='t1_doc_id'",
    )
    .fetch_one(&mut conn)
    .await
    .expect("the concurrent index exists");
    assert!(valid, "the concurrent build completed");

    // A failing no-transaction file is not recorded, and the run stops.
    syn.write(
        "04-broken.sql",
        "-- migrate: no-transaction\nCREATE INDEX CONCURRENTLY t1_broken ON t1 ((nope->>'id'));\n",
    );
    assert!(
        !run(&syn.script(), &[], &url).status.success(),
        "a failing no-transaction file fails the run"
    );
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-table.sql", "02-index.sql", "03-after.sql"],
        "nothing is recorded for a no-transaction file that failed"
    );

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_records_without_running() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &["--baseline"], &url), "--baseline");
    let mut conn = connect(&url).await;
    assert_eq!(recorded(&mut conn).await, vec!["01-first.sql"]);
    assert!(
        !table_exists(&mut conn, "t1").await,
        "--baseline must not execute the migration"
    );

    // A normal run now finds nothing pending: baseline marked it applied.
    assert_ok(&run(&syn.script(), &[], &url), "run after baseline");
    assert!(!table_exists(&mut conn, "t1").await);

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_migration_is_not_recorded_and_stops_the_run() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[
        ("01-ok.sql", "CREATE TABLE t1 (id int);"),
        ("02-broken.sql", "CREATE TALBE t2 (id int);"),
        ("03-after.sql", "CREATE TABLE t3 (id int);"),
    ]);

    let out = run(&syn.script(), &[], &url);
    assert!(!out.status.success(), "a broken migration fails the run");

    let mut conn = connect(&url).await;
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-ok.sql"],
        "the entry before the failure is recorded; the failure and what follows are not"
    );
    assert!(table_exists(&mut conn, "t1").await);
    assert!(
        !table_exists(&mut conn, "t3").await,
        "the run stopped at the failure"
    );

    // Fixing an entry that never applied is legitimate — re-run picks it
    // up along with the rest.
    syn.write("02-broken.sql", "CREATE TABLE t2 (id int);");
    assert_ok(
        &run(&syn.script(), &[], &url),
        "re-run after fixing the broken entry",
    );
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-ok.sql", "02-broken.sql", "03-after.sql"]
    );
    assert!(table_exists(&mut conn, "t3").await);

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_an_applied_migration_fails_the_next_run_by_name() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &[], &url), "applying 01-first");
    let mut conn = connect(&url).await;
    let original = recorded_row(&mut conn, "01-first.sql").await;

    syn.write(
        "01-first.sql",
        "CREATE TABLE t1 (id int);\n-- edited after apply\n",
    );
    let out = run(&syn.script(), &[], &url);
    assert!(
        !out.status.success(),
        "an applied migration whose content changed must fail the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("01-first.sql"),
        "the failure names the drifted file; stderr was:\n{stderr}"
    );
    assert_eq!(
        original,
        recorded_row(&mut conn, "01-first.sql").await,
        "the recorded row keeps the checksum of what actually ran"
    );

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_skips_matching_entries_and_does_not_record_them() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[
        ("01-core.sql", "CREATE TABLE t_core (id int);"),
        ("02-ledger.sql", "CREATE TABLE t_ledger (id int);"),
    ]);

    assert_ok(
        &run(&syn.script(), &["--without", "ledger"], &url),
        "--without ledger",
    );
    let mut conn = connect(&url).await;
    assert!(table_exists(&mut conn, "t_core").await);
    assert!(
        !table_exists(&mut conn, "t_ledger").await,
        "the skipped module did not run"
    );
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-core.sql"],
        "a skipped entry is not recorded — a later full run still owes it"
    );

    assert_ok(
        &run(&syn.script(), &[], &url),
        "full run after a --without run",
    );
    assert!(table_exists(&mut conn, "t_ledger").await);
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-core.sql", "02-ledger.sql"]
    );

    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_db_that_predates_the_runner_is_refused_until_baselined() {
    let (name, url) = scratch_db().await;
    // The marker of a real pre-runner BOSS database: core tables exist,
    // no bookkeeping. audit_log is the sentinel the guard keys on.
    let mut conn = connect(&url).await;
    conn.execute("CREATE TABLE audit_log (id int)")
        .await
        .expect("creating sentinel table");

    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);
    let out = run(&syn.script(), &[], &url);
    assert!(
        !out.status.success(),
        "a populated db with no schema_migrations must not be re-applied from scratch"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--baseline"),
        "the refusal tells the operator the way out; stderr was:\n{stderr}"
    );

    assert_ok(
        &run(&syn.script(), &["--baseline"], &url),
        "--baseline adopts the db",
    );
    assert_ok(
        &run(&syn.script(), &[], &url),
        "normal runs work once baselined",
    );

    drop(conn);
    drop_db(&name);
}

/// Two pods booting together race the schema converge — named as
/// RollingUpdate blocker #2 in boss.yaml's strategy comment. The whole
/// run now holds `pg_advisory_lock` on a dedicated connection and
/// computes its pending set AFTER acquisition, so the loser sees the
/// winner's committed bookkeeping and applies nothing. The lock tag
/// includes the database OID, so TestDb's per-test scratch databases
/// never serialize on each other — only real contenders for one
/// schema do.
///
/// Without the lock this fails loudly: both runs read an empty
/// ledger, race the same non-idempotent CREATE TABLE, and the loser
/// exits nonzero on duplicate DDL or the bookkeeping PK conflict.
#[tokio::test]
async fn two_concurrent_runs_apply_each_migration_exactly_once() {
    let synth = Synthetic::new(&[
        ("001-a.sql", "CREATE TABLE mig_race_a (id INT PRIMARY KEY);"),
        ("002-b.sql", "CREATE TABLE mig_race_b (id INT PRIMARY KEY);"),
        ("003-c.sql", "CREATE TABLE mig_race_c (id INT PRIMARY KEY);"),
    ]);
    let (name, url) = scratch_db().await;

    let script = synth.script();
    let (u1, u2) = (url.clone(), url.clone());
    let s1 = script.clone();
    let t1 = std::thread::spawn(move || run(&s1, &[], &u1));
    let t2 = std::thread::spawn(move || run(&script, &[], &u2));
    let o1 = t1.join().expect("run 1 thread");
    let o2 = t2.join().expect("run 2 thread");

    assert_ok(&o1, "concurrent run 1");
    assert_ok(&o2, "concurrent run 2");

    // "migrate.sh: applied A, already recorded R, of K migrations" —
    // totals across the pair prove nothing ran twice and nothing was
    // skipped: one run applied all three, the other recorded all
    // three as already done.
    let counts = |out: &Output| -> (u32, u32) {
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .rev()
            .find(|l| l.contains("of 3 migrations"))
            .expect("summary line");
        let nums: Vec<u32> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap())
            .collect();
        (nums[0], nums[1])
    };
    let (a1, r1) = counts(&o1);
    let (a2, r2) = counts(&o2);
    assert_eq!(a1 + a2, 3, "each migration applied exactly once");
    assert_eq!(r1 + r2, 3, "the loser saw the winner's bookkeeping");

    let opts = PgConnectOptions::from_str(&url).expect("parsing scratch url");
    let mut conn = PgConnection::connect_with(&opts)
        .await
        .expect("connecting to scratch db");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM schema_migrations")
        .fetch_one(&mut conn)
        .await
        .expect("counting bookkeeping rows");
    assert_eq!(rows, 3, "exactly one bookkeeping row per migration");

    drop(conn);
    drop_db(&name);
}

const FOLD_UNLOGGED: &str = "20261029234727-an-unfinished-fold-writes-no-wal.sql";
const FOLD_EMPTY: &str = "20261029234726-empty-disposable-folds-before-unlogging.sql";

const SEARCH_UNLOGGED: &str = "20261031155000-the-search-cache-writes-no-wal.sql";

fn search_conversion_fixture() -> Synthetic {
    let create = fs::read_to_string(real_schema_dir().join("99-search.sql")).unwrap();
    Synthetic::new(&[(
        "001-search.sql",
        &format!(
            "{create}\n\
        INSERT INTO search_index(ref_kind,ref_id,title,body) VALUES('subject','cacheproof','Cache',repeat('payload ',4096));\n\
        CREATE TABLE cache_originals(id int PRIMARY KEY,payload text);\n\
        INSERT INTO cache_originals VALUES(1,'original-fact');"
        ),
    )])
}

fn install_search_conversion(synth: &Synthetic) {
    // Before the new file exists, the real runner admits no conversion.
    // The behavior assertions below must then fail, not a file-open panic.
    if let Ok(sql) = fs::read_to_string(real_schema_dir().join(SEARCH_UNLOGGED)) {
        synth.write(SEARCH_UNLOGGED, &sql);
    }
}

async fn search_cache_state(conn: &mut PgConnection) -> (String, Vec<String>, String) {
    let persistence = sqlx::query_scalar(
        "SELECT relpersistence::text FROM pg_class WHERE oid='search_index'::regclass",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let rows = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM search_index s ORDER BY ref_kind,ref_id",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    let original = sqlx::query_scalar("SELECT payload FROM cache_originals WHERE id=1")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    (persistence, rows, original)
}

#[tokio::test]
async fn search_conversion_refusals_preserve_the_cache_originals_and_ledger() {
    for publication in [true, false] {
        let synth = search_conversion_fixture();
        let (name, url) = scratch_db().await;
        assert_ok(&run(&synth.script(), &[], &url), "seed search fixture");
        let mut conn = connect(&url).await;
        let before = search_cache_state(&mut conn).await;
        let ledger = recorded(&mut conn).await;
        if publication {
            conn.execute("CREATE PUBLICATION search_cache_fixture FOR TABLE search_index")
                .await
                .expect("isolated fixture owner creates its publication");
        } else {
            conn.execute("CREATE TABLE search_dependency(ref_kind text,ref_id text,FOREIGN KEY(ref_kind,ref_id) REFERENCES search_index); INSERT INTO search_dependency VALUES('subject','cacheproof')")
                .await.unwrap();
        }
        install_search_conversion(&synth);
        let out = run(&synth.script(), &[], &url);
        eprintln!(
            "search refusal publication={publication}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !out.status.success(),
            "a publication or external FK must refuse conversion"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            if publication {
                err.contains("publication") && err.contains("unlogged")
            } else {
                err.contains("foreign key")
            },
            "native refusal must name its cause: {err}"
        );
        assert_eq!(
            search_cache_state(&mut conn).await,
            before,
            "TRUNCATE must roll back if conversion refuses"
        );
        assert_eq!(recorded(&mut conn).await, ledger);
        conn.execute(if publication {
            "DROP PUBLICATION search_cache_fixture"
        } else {
            "DROP TABLE search_dependency"
        })
        .await
        .unwrap();
        assert_ok(
            &run(&synth.script(), &[], &url),
            "retry after removing only fixture obstacle",
        );
        let after = search_cache_state(&mut conn).await;
        assert_eq!(after.0, "u");
        assert!(after.1.is_empty());
        assert_eq!(after.2, before.2);
        assert_eq!(recorded(&mut conn).await.len(), ledger.len() + 1);
        let setting: String = sqlx::query_scalar("SHOW lock_timeout")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(setting, "0", "ordinary file policy must not leak");
        drop(conn);
        drop_db(&name);
    }
}

#[tokio::test]
async fn search_conversion_truncates_before_the_actual_alter_and_rejects_reordering() {
    let actual = fs::read_to_string(real_schema_dir().join(SEARCH_UNLOGGED)).unwrap();
    let commands = "TRUNCATE search_index;\nALTER TABLE search_index SET UNLOGGED;";
    assert!(
        actual.contains(commands),
        "actual migration's two operations are the control input"
    );
    let reordered = actual.replacen(
        commands,
        "ALTER TABLE search_index SET UNLOGGED;\nTRUNCATE search_index;",
        1,
    );
    for (sql, refuse) in [(&actual, false), (&reordered, true)] {
        let synth = search_conversion_fixture();
        let (name, url) = scratch_db().await;
        assert_ok(&run(&synth.script(), &[], &url), "seed populated search");
        let mut conn = connect(&url).await;
        let before = search_cache_state(&mut conn).await;
        conn.execute("CREATE SEQUENCE search_alter_seen; CREATE FUNCTION search_empty_before_alter() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS(SELECT 1 FROM search_index) THEN RAISE EXCEPTION 'search rows present at actual ALTER start'; END IF; PERFORM nextval('search_alter_seen'); END $$; CREATE EVENT TRIGGER search_empty_before_alter ON ddl_command_start WHEN TAG IN ('ALTER TABLE') EXECUTE FUNCTION search_empty_before_alter();")
            .await.expect("owned isolated DDL-start witness");
        synth.write(SEARCH_UNLOGGED, sql);
        let out = run(&synth.script(), &[], &url);
        eprintln!(
            "search ordering refuse={refuse}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if refuse {
            assert!(!out.status.success());
            assert!(
                String::from_utf8_lossy(&out.stderr)
                    .contains("search rows present at actual ALTER start")
            );
            assert_eq!(search_cache_state(&mut conn).await, before);
            assert_eq!(recorded(&mut conn).await, ["001-search.sql"]);
        } else {
            assert_ok(&out, "empty conversion");
            let witnessed: bool = sqlx::query_scalar("SELECT is_called FROM search_alter_seen")
                .fetch_one(&mut conn)
                .await
                .unwrap();
            assert!(witnessed, "actual ALTER reached the empty-table witness");
            let after = search_cache_state(&mut conn).await;
            assert_eq!(after.0, "u");
            assert!(after.1.is_empty());
            assert_eq!(after.2, before.2);
            assert_ok(
                &run(&synth.script(), &[], &url),
                "recorded retry is idempotent",
            );
            assert_eq!(recorded(&mut conn).await.len(), 2);
        }
        drop(conn);
        drop_db(&name);
    }
}

#[tokio::test]
async fn search_conversion_cancels_the_active_exclusive_rebuild_before_truncating() {
    let synth = search_conversion_fixture();
    let (name, url) = scratch_db().await;
    assert_ok(
        &run(&synth.script(), &[], &url),
        "seed active rebuild fixture",
    );
    let mut conn = connect(&url).await;
    let original = search_cache_state(&mut conn).await.2;
    let mut blocker = connect(&url).await;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .unwrap();
    blocker
        .execute("BEGIN; LOCK TABLE search_index IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let sleeping = tokio::spawn(async move {
        let result = blocker.execute("SELECT pg_sleep(30)").await;
        let code = result.err().and_then(|e| {
            e.as_database_error()
                .and_then(|e| e.code().map(|c| c.to_string()))
        });
        blocker.execute("ROLLBACK").await.unwrap();
        code
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE l.pid=$1 AND l.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND l.relation='search_index'::regclass AND l.locktype='relation' AND l.mode='AccessExclusiveLock' AND l.granted AND a.state='active' AND a.wait_event='PgSleep')")
                .bind(pid).fetch_one(&mut conn).await.unwrap();
            if active { break; }
            assert!(!sleeping.is_finished(), "blocker must still own the actual lock");
            tokio::task::yield_now().await;
        }
    }).await.expect("actual active rebuild lock witness");
    install_search_conversion(&synth);
    let script = synth.script();
    let target = url.clone();
    let out = tokio::task::spawn_blocking(move || run(&script, &[], &target))
        .await
        .unwrap();
    let needed_fixture_cleanup = !sleeping.is_finished();
    if needed_fixture_cleanup {
        sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(pid)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let code = sleeping.await.unwrap();
    eprintln!(
        "exclusive rebuild migration cleanup={needed_fixture_cleanup} blocker={code:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ok(
        &out,
        "migration cancels the actual active exclusive rebuild",
    );
    assert_eq!(code.as_deref(), Some("57014"), "cancel, never terminate");
    let after = search_cache_state(&mut conn).await;
    assert_eq!(after.0, "u");
    assert!(after.1.is_empty());
    assert_eq!(after.2, original);
    assert_eq!(recorded(&mut conn).await.len(), 2);
    assert_ok(
        &run(&synth.script(), &[], &url),
        "recorded retry is idempotent",
    );
    drop(conn);
    drop_db(&name);
}

#[tokio::test]
async fn search_conversion_lock_refusal_is_bounded_atomic_and_retryable() {
    let synth = search_conversion_fixture();
    let (name, url) = scratch_db().await;
    assert_ok(&run(&synth.script(), &[], &url), "seed lock fixture");
    let mut conn = connect(&url).await;
    let before = search_cache_state(&mut conn).await;
    let mut blocker = connect(&url).await;
    blocker
        .execute("BEGIN; LOCK TABLE search_index IN ACCESS SHARE MODE")
        .await
        .unwrap();
    install_search_conversion(&synth);
    let script = synth.script();
    let target = url.clone();
    let thread = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args(["--", "psql", &target, "-v", "VERBOSITY=verbose"])
            .env("PGOPTIONS", "-c statement_timeout=8000")
            .output()
            .unwrap()
    });
    let observed=tokio::time::timeout(std::time::Duration::from_secs(15),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND relation='search_index'::regclass AND NOT granted)").fetch_one(&mut conn).await.unwrap();
            if waiting {break;} assert!(!thread.is_finished(),"runner ended without an actual pending search lock");tokio::task::yield_now().await;
        }
    }).await;
    let out = tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    blocker.execute("ROLLBACK").await.unwrap();
    observed.expect("physical relation lock witness");
    eprintln!(
        "search lock stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("55P03"),
        "not the outer statement timeout: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(search_cache_state(&mut conn).await, before);
    assert_eq!(recorded(&mut conn).await, ["001-search.sql"]);
    assert_ok(&run(&synth.script(), &[], &url), "retry after lock release");
    assert_eq!(search_cache_state(&mut conn).await.0, "u");
    drop(blocker);
    drop(conn);
    drop_db(&name);
}

#[tokio::test]
async fn an_applied_fold_conversion_is_not_followed_by_a_late_progress_truncation() {
    let synth = fold_conversion_fixture();
    let late = synth.dir.join("schema").join(FOLD_EMPTY);
    if late.exists() {
        fs::remove_file(&late).unwrap();
    }
    let (name, url) = scratch_db().await;
    assert_ok(
        &run(&synth.script(), &[], &url),
        "record original conversion",
    );
    let mut conn = connect(&url).await;
    let before = recorded(&mut conn).await;
    assert!(before.iter().any(|id| id == FOLD_UNLOGGED));
    // A new file is applied even when it sorts before an already recorded
    // migration. Use the candidate bytes, not a hand-written TRUNCATE.
    if let Ok(sql) = fs::read_to_string(real_schema_dir().join(FOLD_EMPTY)) {
        synth.write(FOLD_EMPTY, &sql);
    }
    assert_ok(
        &run(&synth.script(), &[], &url),
        "retry after prior conversion",
    );
    let rows: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM membership_fold_builds)+(SELECT count(*) FROM membership_fold_build_chunks)")
        .fetch_one(&mut conn).await.unwrap();
    assert_eq!(
        rows, 2,
        "an already converted database must retain its progress"
    );
    let served: String = sqlx::query_scalar("SELECT payload FROM served WHERE id=1")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(served, "served-generation");
    assert_eq!(
        recorded(&mut conn).await,
        before,
        "no late destructive migration"
    );
    drop(conn);
    drop_db(&name);
}

fn fold_conversion_fixture() -> Synthetic {
    let schema = real_schema_dir();
    let create =
        fs::read_to_string(schema.join("20261029015000-membership-fold-builds.sql")).unwrap();
    let synth = Synthetic::new(&[(
        "001-fold.sql",
        &format!(
            "{create}\nCREATE TABLE served(id int PRIMARY KEY, payload text);\n\
         INSERT INTO served VALUES(1,'served-generation');\n\
         INSERT INTO membership_fold_builds VALUES('disposable',1,0,NULL,'digest',1048576);\n\
         INSERT INTO membership_fold_build_chunks VALUES('disposable',0,decode(repeat('ab',1048576),'hex'));"
        ),
    )]);
    synth.write(
        FOLD_UNLOGGED,
        &fs::read_to_string(schema.join(FOLD_UNLOGGED)).unwrap(),
    );
    synth
}

/// 78625ebd: a concurrent index waits for an older snapshot even when
/// that reader never touches the indexed table. A post-start failure
/// leaves debt; retry removes the INVALID index without rewriting SQL.
#[tokio::test(flavor = "multi_thread")]
async fn the_original_deferred_index_timeout_is_not_recorded_as_success() {
    let sql = fs::read_to_string(
        real_schema_dir().join("20261031231300-last-firings-walk-their-order.sql"),
    )
    .unwrap();
    let syn = Synthetic::new(&[(
        "001-fixture.sql",
        "CREATE TABLE dispatcher_firings(rule_name text, fired_at timestamptz, firing_id text, outcome text);\n\
         INSERT INTO dispatcher_firings VALUES('rule',now(),'id','fired');\n\
         CREATE TABLE unrelated_snapshot(id int); INSERT INTO unrelated_snapshot VALUES(1);",
    )]);
    let (name, url) = scratch_db().await;
    assert_ok(&run(&syn.script(), &[], &url), "create recorded fixture");
    let mut blocker = connect(&url).await;
    blocker
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM unrelated_snapshot")
        .await
        .unwrap();
    syn.write("002-last-firings.sql", &sql);
    syn.phase("002-last-firings.sql", "post-start");
    let script = syn.script();
    let target = url.clone();
    let runner = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args([
                "--phase",
                "post-start",
                "--",
                "psql",
                &target,
                "-v",
                "VERBOSITY=verbose",
            ])
            // Fixture containment only: the actual migration owns its
            // lock policy, and a broken fixture must still terminate.
            .env("PGOPTIONS", "-c statement_timeout=25000")
            .output()
            .unwrap()
    });
    let mut observer = connect(&url).await;
    let waiting = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_progress_create_index \
                 WHERE datid=(SELECT oid FROM pg_database WHERE datname=current_database()) \
                 AND phase='waiting for old snapshots')",
            )
            .fetch_one(&mut observer)
            .await
            .unwrap();
            if pending {
                break;
            }
            assert!(
                !runner.is_finished(),
                "migration ended before the actual old-snapshot wait"
            );
            tokio::task::yield_now().await;
        }
    })
    .await;
    if waiting.is_ok() {
        // Keep the witnessed old snapshot open past the original SQL's
        // five-second lock_timeout. PostgreSQL, not a host elapsed-time
        // assertion, establishes that interval.
        observer.execute("SELECT pg_sleep(6)").await.unwrap();
    }
    blocker.execute("ROLLBACK").await.unwrap();
    let out = tokio::task::spawn_blocking(move || runner.join().unwrap())
        .await
        .unwrap();
    waiting.expect("actual concurrent-index old-snapshot phase");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("55P03"));
    let valid: bool = sqlx::query_scalar(
        "SELECT indisvalid FROM pg_index \
         WHERE indexrelid='dispatcher_firings_newest_fired'::regclass",
    )
    .fetch_one(&mut observer)
    .await
    .unwrap();
    assert!(
        !valid,
        "the failed original build leaves an INVALID index to retry"
    );
    assert_eq!(recorded(&mut observer).await, ["001-fixture.sql"]);
    assert_eq!(
        run(&syn.script(), &["--phase", "post-start", "--pending"], &url)
            .status
            .code(),
        Some(75)
    );
    assert_ok(
        &run(&syn.script(), &["--phase", "post-start"], &url),
        "retry unchanged SQL after the snapshot closes",
    );
    let valid: bool = sqlx::query_scalar("SELECT indisvalid FROM pg_index WHERE indexrelid='dispatcher_firings_newest_fired'::regclass").fetch_one(&mut observer).await.unwrap();
    assert!(valid);
    assert_eq!(
        recorded(&mut observer).await,
        ["001-fixture.sql", "002-last-firings.sql"]
    );
    assert_eq!(
        fs::read_to_string(syn.dir.join("schema/002-last-firings.sql")).unwrap(),
        sql
    );
    drop(observer);
    drop(blocker);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_start_work_is_deferred_at_boot_and_retried_without_rewriting_history() {
    let file = "20261031231300-last-firings-walk-their-order.sql";
    let original = fs::read_to_string(real_schema_dir().join(file)).unwrap();
    let syn = Synthetic::new(&[(
        "001-schema.sql",
        "CREATE TABLE dispatcher_firings(rule_name text, fired_at timestamptz, firing_id text, outcome text); CREATE TABLE ordinary(id int);",
    )]);
    let (name, url) = scratch_db().await;
    assert_ok(&run(&syn.script(), &[], &url), "fixture schema");
    syn.write(file, &original);
    syn.phase(file, "post-start");
    syn.write(
        "20261101000100-after.sql",
        "INSERT INTO ordinary VALUES(7);",
    );
    let mut blocker = connect(&url).await;
    blocker
        .execute("BEGIN; INSERT INTO dispatcher_firings VALUES('rule',now(),'held','fired')")
        .await
        .unwrap();
    let script = syn.script();
    let target = url.clone();
    let boot = tokio::task::spawn_blocking(move || run(&script, &["--phase", "boot"], &target))
        .await
        .unwrap();
    // Release before asserting even on the original runner's absent port.
    blocker.execute("ROLLBACK").await.unwrap();
    assert_ok(&boot, "BOOT must serve while optional CIC is deferred");
    assert!(String::from_utf8_lossy(&boot.stdout).contains(&format!("deferred {file}")));
    let mut observer = connect(&url).await;
    assert_eq!(
        recorded(&mut observer).await,
        ["001-schema.sql", "20261101000100-after.sql"]
    );
    let pending = run(&syn.script(), &["--phase", "post-start", "--pending"], &url);
    assert_eq!(pending.status.code(), Some(75));
    assert!(String::from_utf8_lossy(&pending.stdout).contains(file));
    assert!(!table_exists(&mut observer, "dispatcher_firings_newest_fired").await);
    let value: i32 = sqlx::query_scalar("SELECT id FROM ordinary")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(value, 7, "a deferred entry cannot skip later normal schema");
    assert_eq!(
        fs::read_to_string(syn.dir.join("schema").join(file)).unwrap(),
        original
    );
    assert_ok(
        &run(&syn.script(), &["--phase", "post-start"], &url),
        "post-start after blocker release",
    );
    let valid: bool = sqlx::query_scalar("SELECT indisvalid FROM pg_index WHERE indexrelid='dispatcher_firings_newest_fired'::regclass").fetch_one(&mut observer).await.unwrap();
    assert!(valid);
    let before = recorded_row(&mut observer, file).await;
    assert_ok(
        &run(&syn.script(), &["--phase", "post-start"], &url),
        "idempotent post-start replay",
    );
    assert_eq!(before, recorded_row(&mut observer, file).await);
    assert_ok(
        &run(&syn.script(), &["--phase", "post-start", "--pending"], &url),
        "paid post-start debt",
    );
    syn.write(file, &(original + "\n-- changed history\n"));
    let changed = run(&syn.script(), &["--phase", "boot"], &url);
    assert!(
        !changed.status.success(),
        "BOOT may not hide changed deferred history"
    );
    drop(observer);
    drop(blocker);
    drop_db(&name);
}

#[tokio::test]
async fn post_start_registry_and_phase_refuse_unknown_or_drifted_input() {
    let syn = Synthetic::new(&[("001-schema.sql", "CREATE TABLE ordinary(id int);")]);
    let (name, url) = scratch_db().await;
    let unknown = run(&syn.script(), &["--phase", "unknown"], &url);
    assert!(!unknown.status.success());
    let pending_boot = run(&syn.script(), &["--phase", "post-start"], &url);
    assert!(!pending_boot.status.success());
    assert!(
        String::from_utf8_lossy(&pending_boot.stderr)
            .contains("requires recorded boot schema: 001-schema.sql")
    );
    syn.phase("001-schema.sql", "unknown");
    assert!(
        !run(&syn.script(), &["--phase", "boot"], &url)
            .status
            .success()
    );
    syn.phase("001-schema.sql", "post-start");
    syn.write("001-schema.sql", "CREATE TABLE different(id int);");
    assert!(
        !run(&syn.script(), &["--phase", "boot"], &url)
            .status
            .success()
    );
    let mut conn = connect(&url).await;
    assert!(!table_exists(&mut conn, "ordinary").await);
    assert!(!table_exists(&mut conn, "different").await);
    drop(conn);
    drop_db(&name);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_start_waits_serialize_with_each_other_without_owning_the_boot_lock() {
    let syn = Synthetic::new(&[(
        "001-schema.sql",
        "CREATE TABLE target(id int); CREATE TABLE unrelated(id int); INSERT INTO unrelated VALUES(1);",
    )]);
    let (name, url) = scratch_db().await;
    assert_ok(&run(&syn.script(), &[], &url), "fixture schema");
    syn.write(
        "002-index.sql",
        "-- migrate: no-transaction\nCREATE INDEX CONCURRENTLY target_id ON target(id);\n",
    );
    syn.phase("002-index.sql", "post-start");
    let mut blocker = connect(&url).await;
    blocker
        .execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM unrelated")
        .await
        .unwrap();
    let script = syn.script();
    let target = url.clone();
    let first = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args(["--phase", "post-start", "--", "psql", &target])
            .env("PGOPTIONS", "-c statement_timeout=60000")
            .output()
            .unwrap()
    });
    let mut observer = connect(&url).await;
    let phase=tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_progress_create_index WHERE datid=(SELECT oid FROM pg_database WHERE datname=current_database()) AND phase='waiting for old snapshots')").fetch_one(&mut observer).await.unwrap();
            if waiting {return true;}
            if first.is_finished() {return false;}
            tokio::task::yield_now().await;
        }
    }).await;
    let script = syn.script();
    let target = url.clone();
    let second = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args(["--phase", "post-start", "--", "psql", &target])
            .env("PGOPTIONS", "-c statement_timeout=60000")
            .output()
            .unwrap()
    });
    let serialized=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND locktype='advisory' AND objid=477201127 AND NOT granted)").fetch_one(&mut observer).await.unwrap();
            if waiting {return true;}
            if second.is_finished() {return false;}
            tokio::task::yield_now().await;
        }
    }).await;
    syn.write("003-later.sql", "INSERT INTO unrelated VALUES(2);");
    let script = syn.script();
    let target = url.clone();
    let boot = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args([
                "--phase",
                "boot",
                "--",
                "psql",
                "-v",
                "ON_ERROR_STOP=1",
                &target,
            ])
            .env("PGOPTIONS", "-c statement_timeout=60000")
            .env("PGAPPNAME", "cic_fixture_boot")
            .output()
            .unwrap()
    });
    let boot_before_release=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        loop {
            if boot.is_finished() {return true;}
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE a.datname=current_database() AND a.application_name='cic_fixture_boot' AND l.locktype='advisory' AND l.objid=477201126 AND NOT l.granted)").fetch_one(&mut observer).await.unwrap();
            if waiting {return false;}
            tokio::task::yield_now().await;
        }
    }).await;
    blocker.execute("ROLLBACK").await.unwrap();
    let (one, two, boot) = tokio::task::spawn_blocking(move || {
        (
            first.join().unwrap(),
            second.join().unwrap(),
            boot.join().unwrap(),
        )
    })
    .await
    .unwrap();
    assert!(phase.unwrap(), "actual post-start old-snapshot wait");
    assert!(
        boot_before_release.unwrap(),
        "BOOT completed before releasing the post-start blocker, rather than waiting on its advisory lock"
    );
    assert_ok(&boot, "BOOT while post-start waits on an old snapshot");
    assert!(
        serialized.unwrap(),
        "second post-start owns no SQL while waiting"
    );
    assert_ok(&one, "first post-start owner");
    assert_ok(&two, "second post-start reads the committed ledger");
    assert_eq!(recorded(&mut observer).await.len(), 3);
    let value: i64 = sqlx::query_scalar("SELECT count(*) FROM unrelated")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(value, 2);
    drop(observer);
    drop(blocker);
    drop_db(&name);
}

#[test]
fn every_concurrent_index_declares_its_long_transaction_phase_and_debt_owner() {
    let root = boss_testing::repo_root();
    let phases = root.join("infra/postgres/migration-phases");
    let mut concurrent = 0;
    for entry in fs::read_dir(real_schema_dir()).unwrap() {
        let path = entry.unwrap().path();
        let sql = fs::read_to_string(&path).unwrap();
        let words: Vec<_> = sql.split_whitespace().collect();
        if !words
            .windows(3)
            .any(|part| part == ["CREATE", "INDEX", "CONCURRENTLY"])
        {
            continue;
        }
        concurrent += 1;
        let name = path.file_name().unwrap().to_str().unwrap();
        let declaration = fs::read_to_string(phases.join(format!("{name}.phase"))).unwrap();
        let fields: Vec<_> = declaration.split_whitespace().collect();
        assert_eq!(fields.len(), 2, "{name}");
        let output = std::process::Command::new("sha256sum")
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            fields[1],
            String::from_utf8(output.stdout)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap(),
            "{name}"
        );
        assert_eq!(sql.lines().next(), Some("-- migrate: no-transaction"));
        match fields[0] {
            "boot-wait" => assert!(
                !sql.contains("SET lock_timeout"),
                "{name}: BOOT CIC cannot cancel its old-snapshot wait"
            ),
            "post-start" => assert_eq!(name, "20261031231300-last-firings-walk-their-order.sql"),
            other => panic!("unknown phase {other} for {name}"),
        }
    }
    assert!(concurrent > 0);
    assert_eq!(
        fs::read_dir(&phases).unwrap().count(),
        concurrent,
        "phase registry contains exactly the concurrent-index entries"
    );
    let init = fs::read_to_string(root.join("infra/oss-quickstart/init.sh")).unwrap();
    assert!(init.contains("\"$REPO/infra/postgres/migrate.sh\" --phase boot"));
    let manifest =
        fs::read_to_string(root.join("infra/cluster/manifests/boss-post-start-schema.yaml"))
            .expect("deferred schema needs a supervised image-pinned retry owner");
    for contract in [
        "kind: CronJob",
        "boss-chore: \"true\"",
        "concurrencyPolicy: Forbid",
        "activeDeadlineSeconds:",
        "name: chore",
        "/opt/boss/infra/postgres/post-start-bound.py",
        "{name: BOSS_CHORE_FULL_OUTPUT, value: \"1\"}",
    ] {
        assert!(
            manifest.contains(contract),
            "missing supervised post-start contract: {contract}"
        );
    }
    let bound = fs::read_to_string(root.join("infra/postgres/post-start-bound.py")).unwrap();
    assert!(bound.contains("with_name(\"post-start.sh\")"));
    let workflow = fs::read_to_string(
        root.join("infra/platform/workflows/maintenance-post-start-schema.toml"),
    )
    .unwrap();
    assert!(workflow.contains("kind = \"maintenance-post-start-schema\""));
}

#[tokio::test]
async fn post_start_pending_refuses_a_failed_ledger_reader_after_partial_output() {
    let syn = Synthetic::new(&[("001-schema.sql", "CREATE TABLE target(id int);")]);
    syn.write(
        "002-index.sql",
        "-- migrate: no-transaction\nCREATE INDEX CONCURRENTLY target_id ON target(id);\n",
    );
    syn.phase("002-index.sql", "post-start");
    let (name, url) = scratch_db().await;
    assert_ok(
        &run(&syn.script(), &[], &url),
        "record both fixture migrations",
    );
    assert_ok(
        &run(&syn.script(), &["--phase", "post-start", "--pending"], &url),
        "positive paid-debt reader control",
    );
    let shim = syn.dir.join("ledger-read-then-fail.sh");
    fs::write(
        &shim,
        r#"#!/usr/bin/env bash
set -u
for arg in "$@"; do
    if [ "$arg" = "SELECT id || '|' || checksum FROM schema_migrations" ]; then
        psql "$@" || exit $?
        echo 'injected ledger reader failure after real database rows' >&2
        exit 42
    fi
done
exec psql "$@"
"#,
    )
    .unwrap();
    let mut conn = connect(&url).await;
    let before = recorded(&mut conn).await;
    let out = std::process::Command::new("bash")
        .arg(syn.script())
        .args(["--phase", "post-start", "--pending", "--", "bash"])
        .arg(&shim)
        .arg(&url)
        .output()
        .unwrap();
    let after = recorded(&mut conn).await;
    drop(conn);
    drop_db(&name);
    eprintln!(
        "fault-injected ledger reader: status={}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(before, after, "fault injection changes no ledger rows");
    assert!(
        !out.status.success(),
        "a failed real-ledger reader cannot report paid debt"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("injected ledger reader failure after real database rows"));
    assert!(stderr.contains("schema_migrations ledger read failed"));
    assert!(
        stderr.contains("001-schema.sql|") && stderr.contains("002-index.sql|"),
        "partial rows remain in the failure evidence"
    );
}

#[tokio::test]
async fn an_ordinary_migration_bounds_even_an_already_unlogged_alter_lock_wait() {
    let synth = Synthetic::new(&[(
        "001-target.sql",
        "CREATE UNLOGGED TABLE target(id int); INSERT INTO target VALUES(7);",
    )]);
    let (name, url) = scratch_db().await;
    assert_ok(
        &run(&synth.script(), &[], &url),
        "create already unlogged target",
    );
    synth.write("002-noop.sql", "ALTER TABLE target SET UNLOGGED;");
    let mut blocker = connect(&url).await;
    blocker
        .execute("BEGIN; LOCK TABLE target IN ACCESS SHARE MODE")
        .await
        .unwrap();
    let script = synth.script();
    let target_url = url.clone();
    // Harness-only outer statement bound ensures the original runner also
    // terminates. Its 57014 differs from the required lock refusal 55P03.
    let thread = std::thread::spawn(move || {
        std::process::Command::new("bash")
            .arg(script)
            .args(["--", "psql", &target_url, "-v", "VERBOSITY=verbose"])
            .env("PGOPTIONS", "-c statement_timeout=8000")
            .output()
            .unwrap()
    });
    let mut observer = connect(&url).await;
    let observed = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND relation='target'::regclass AND NOT granted)")
                .fetch_one(&mut observer).await.unwrap();
            if waiting { break; }
            assert!(!thread.is_finished(), "runner ended without an observed target lock wait");
            tokio::task::yield_now().await;
        }
    }).await;
    let out = tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    blocker.execute("ROLLBACK").await.unwrap();
    observed.expect("actual pending relation lock");
    eprintln!(
        "held-lock runner stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "held target must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("55P03"),
        "must refuse on bounded lock wait, not outer statement bound:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(recorded(&mut observer).await, ["001-target.sql"]);
    let row: i32 = sqlx::query_scalar("SELECT id FROM target")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(row, 7);
    assert_ok(
        &run(&synth.script(), &[], &url),
        "retry after releasing blocker",
    );
    let setting: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert_eq!(
        setting, "0",
        "no database, role, or other-session timeout leak"
    );
    drop(observer);
    drop(blocker);
    drop_db(&name);
}

#[tokio::test]
async fn ordinary_migration_lock_bound_is_transaction_local_not_a_session_policy() {
    let synth = Synthetic::new(&[
        (
            "001-setting.sql",
            "CREATE TABLE observed(id int, value text); INSERT INTO observed SELECT 1,current_setting('lock_timeout');",
        ),
        (
            "002-setting.sql",
            "INSERT INTO observed SELECT 2,current_setting('lock_timeout');",
        ),
        (
            "003-outside.sql",
            "-- migrate: no-transaction\nINSERT INTO observed SELECT 3,current_setting('lock_timeout');",
        ),
    ]);
    let (name, url) = scratch_db().await;
    assert_ok(
        &run(&synth.script(), &[], &url),
        "per-file timeout observations",
    );
    let mut conn = connect(&url).await;
    let settings: Vec<String> = sqlx::query_scalar("SELECT value FROM observed ORDER BY id")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        settings,
        ["5s", "5s", "0"],
        "every ordinary transaction is bounded; no-transaction keeps its existing behavior"
    );
    let setting: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(setting, "0");
    drop(conn);
    drop_db(&name);
}
