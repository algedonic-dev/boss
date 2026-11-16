//! tree-wide pin — the server declares dead-client prevention independently of a pool.
#[path = "support/postgres_disconnect_settings.rs"]
mod postgres_disconnect_settings;
use postgres_disconnect_settings::{
    declared_settings, manifest_text, postgres_container, settings_in,
};

/// Every setting the jobs database is started with, each declared ONCE here
/// and once in the manifest; the pin below holds the two equal, so a setting
/// added to either alone is named.
///
/// - max_connections: ~24 API services each hold an sqlx pool (default 10).
/// - client_connection_check_interval and tcp_keepalives_*: a rolled-away
///   client must not leave its query running for hours (bbf242ed). These are
///   disconnect defaults, not a statement timeout.
/// - track_io_timing and track_wal_io_timing (d9b39685 / 923d75bd): four
///   hours of slow-request moments named LWLock:WALWrite and no lock wait,
///   but with both switches off the statistics could name the wait and not
///   time it.
/// - shared_buffers, wal_buffers, max_wal_size, min_wal_size and
///   wal_compression (d9b39685, option A, David 2026-10-29): with those
///   switches on, each slow moment timed WAL writes of seconds at the
///   storage layer. Fewer checkpoints mean fewer full-page images, lz4
///   shrinks the ones left, and a larger WAL buffer lets a backend keep
///   inserting while a write is stalled. The manifest says why each number.
/// - shared_preload_libraries (891e48c9, conductor 2026-10-29): temp-file
///   spills may outweigh WAL on the same disks, and naming the statements
///   that spill needs pg_stat_statements, which loads only at start. It
///   rides the tuning car so the database restarts once for both.
const DECLARED: [(&str, &str); 13] = [
    ("max_connections", "400"),
    ("client_connection_check_interval", "10s"),
    ("tcp_keepalives_idle", "60"),
    ("tcp_keepalives_interval", "10"),
    ("tcp_keepalives_count", "3"),
    ("track_io_timing", "on"),
    ("track_wal_io_timing", "on"),
    ("shared_buffers", "2GB"),
    ("wal_buffers", "64MB"),
    ("max_wal_size", "4GB"),
    ("min_wal_size", "1GB"),
    ("wal_compression", "lz4"),
    ("shared_preload_libraries", "pg_stat_statements"),
];

/// The memory the database container requests, declared once here and held
/// equal to the manifest. Review of this car (d9b39685, conductor
/// 2026-10-29T21:14Z) made raising it from 1Gi REQUIRED before it lands: the
/// pod declares no limit, so a pod using well over its request is among the
/// first a node under memory pressure evicts, and shared_buffers alone is now
/// 2GB. The manifest says how the number was checked against the nodes.
const MEMORY_REQUEST: &str = "3Gi";

/// What a backend needs beyond shared memory, at least: 400 connections'
/// private memory, work_mem sorts and autovacuum. A request that only just
/// covers shared_buffers puts the pod over it the moment it serves.
const MEMORY_BEYOND_SHARED_BUFFERS: u64 = 1 << 30;

/// Bytes in a Kubernetes binary quantity (`3Gi`) or a PostgreSQL memory
/// setting (`2GB`, whose units are 1024-based); any other spelling is refused
/// rather than read as zero.
fn bytes(quantity: &str) -> u64 {
    [
        ("Gi", 1u64 << 30),
        ("Mi", 1 << 20),
        ("GB", 1 << 30),
        ("MB", 1 << 20),
    ]
    .iter()
    .find_map(|(unit, scale)| {
        quantity
            .strip_suffix(unit)
            .and_then(|n| n.parse::<u64>().ok())
            .map(|n| n * scale)
    })
    .unwrap_or_else(|| panic!("{quantity:?} is not a quantity this pin reads"))
}

fn memory_request_in(manifest: &str) -> Option<String> {
    let container = postgres_container(manifest).unwrap();
    container["resources"]["requests"]["memory"]
        .as_str()
        .map(str::to_owned)
}

/// The schema files every database is built from, in no particular order.
fn schema_sql() -> Vec<(String, String)> {
    let dir = boss_testing::repo_root().join("infra/postgres/schema");
    std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            (path.display().to_string(), text)
        })
        .collect()
}

/// Durability is a remedy decision with a data-loss cost, and David chose
/// against changing it (d9b39685, option A, 2026-10-29): each of these stays
/// at the server's default, in every spelling PostgreSQL accepts.
const DURABILITY: [&str; 4] = [
    "synchronous_commit",
    "fsync",
    "full_page_writes",
    "wal_sync_method",
];

fn durability_declared(settings: &[(String, String)]) -> Vec<&str> {
    settings
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| DURABILITY.contains(name))
        .collect()
}

/// The real manifest with `extra` appended to the postgres args, in the shape
/// the file actually has (one `- arg` line each, after the last setting).
fn manifest_with(extra: &[&str]) -> String {
    let anchor = "            - track_wal_io_timing=on\n";
    let manifest = manifest_text();
    assert!(manifest.contains(anchor), "the fixture anchor moved");
    let added: String = extra
        .iter()
        .map(|arg| format!("            - {arg}\n"))
        .collect();
    manifest.replacen(anchor, &format!("{anchor}{added}"), 1)
}

#[test]
fn postgres_is_started_with_each_declared_setting_once_and_nothing_else() {
    let mut declared = declared_settings();
    declared.sort();
    let mut expected: Vec<(String, String)> = DECLARED
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    expected.sort();
    assert_eq!(declared, expected);
}

/// A preloaded library that is an extension answers nothing until a database
/// creates it: pg_stat_statements loaded with no view to read is a restart
/// paid for no reading (891e48c9). So every library the server preloads is
/// created by a schema migration, as its own statement, in every database
/// the schema builds; a comment that names it does not count.
#[test]
fn every_preloaded_library_is_created_by_a_migration() {
    let declared = declared_settings();
    let libraries: Vec<&str> = declared
        .iter()
        .filter(|(name, _)| name == "shared_preload_libraries")
        .flat_map(|(_, value)| value.split(','))
        .map(str::trim)
        .filter(|library| !library.is_empty())
        .collect();
    assert!(
        !libraries.is_empty(),
        "the manifest preloads no library, so this pin reads nothing"
    );
    let schema = schema_sql();
    for library in libraries {
        let statement = format!("CREATE EXTENSION IF NOT EXISTS {library};");
        let creators: Vec<&str> = schema
            .iter()
            .filter(|(_, text)| text.lines().any(|line| line.trim() == statement))
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(
            creators.len(),
            1,
            "{library} is preloaded; exactly one schema file must say {statement:?}, found {creators:?}"
        );
    }
}

#[test]
fn postgres_requests_the_memory_declared_here() {
    assert_eq!(
        memory_request_in(&manifest_text()).as_deref(),
        Some(MEMORY_REQUEST)
    );
}

/// shared_buffers is allocated at start and never given back, so a request
/// below it plus a backend's working room makes the database an early
/// eviction candidate by its own configuration. Raising shared_buffers past
/// the request fails here, naming both numbers.
#[test]
fn postgres_requests_more_memory_than_its_shared_buffers_need() {
    // Both numbers off the manifest, not the constants above: the relation
    // is the manifest's own, so it holds whatever the equality pins say.
    let shared_buffers = declared_settings()
        .iter()
        .find(|(name, _)| name == "shared_buffers")
        .map(|(_, value)| bytes(value))
        .expect("shared_buffers is declared");
    let written = memory_request_in(&manifest_text()).expect("the database requests memory");
    let request = bytes(&written);
    assert!(
        request >= shared_buffers + MEMORY_BEYOND_SHARED_BUFFERS,
        "the request {written} must cover shared_buffers ({shared_buffers} bytes) plus {MEMORY_BEYOND_SHARED_BUFFERS} bytes"
    );
}

#[test]
fn a_memory_quantity_is_read_in_bytes_or_refused() {
    assert_eq!(bytes("3Gi"), 3 << 30);
    assert_eq!(bytes("3072Mi"), 3 << 30);
    assert_eq!(bytes("2GB"), 2 << 30);
    assert_eq!(bytes("128MB"), 128 << 20);
    for unread in ["3G", "3", "1.5Gi", "Gi"] {
        assert!(
            std::panic::catch_unwind(|| bytes(unread)).is_err(),
            "{unread} must be refused"
        );
    }
}

#[test]
fn postgres_declares_finite_dead_client_detection_without_a_global_query_timeout() {
    assert!(
        !DECLARED
            .iter()
            .any(|(name, _)| *name == "statement_timeout")
    );
    assert!(
        !declared_settings()
            .iter()
            .any(|(name, _)| name == "statement_timeout")
    );
}

#[test]
fn postgres_leaves_durability_at_its_defaults() {
    assert!(
        DECLARED.iter().all(|(name, _)| !DURABILITY.contains(name)),
        "a durability setting is not a tuning setting"
    );
    assert_eq!(
        durability_declared(&declared_settings()),
        Vec::<&str>::new()
    );
}

/// 1702a075 (review 2090c80f of car f057697c): PostgreSQL reads a setting
/// name case-insensitively and its start-up parser (ParseLongOption) reads
/// `-` as `_`, so each of these turns a durability switch off while a pin
/// comparing the exact lowercase spelling passed it.
#[test]
fn a_durability_setting_is_refused_in_every_spelling_postgres_accepts() {
    for (spelling, name) in [
        ("synchronous-commit=off", "synchronous_commit"),
        ("FSYNC=off", "fsync"),
        ("Full_Page_Writes=off", "full_page_writes"),
        ("WAL-Sync-Method=open_datasync", "wal_sync_method"),
    ] {
        let settings = settings_in(&manifest_with(&["-c", spelling]))
            .unwrap_or_else(|refusal| panic!("{spelling}: {refusal}"));
        assert_eq!(durability_declared(&settings), [name], "{spelling}");
    }
}

/// A setting declared twice under two spellings is one setting declared
/// twice: PostgreSQL applies the last, so the pin would read the first.
#[test]
fn a_setting_declared_twice_in_any_spelling_is_refused() {
    for (spelling, name) in [
        ("Track_WAL_IO_Timing=off", "track_wal_io_timing"),
        ("max-connections=100", "max_connections"),
        ("track_io_timing=off", "track_io_timing"),
    ] {
        let refusal = settings_in(&manifest_with(&["-c", spelling]))
            .expect_err(&format!("{spelling} declares {name} a second time"));
        assert!(refusal.contains(name), "{spelling}: {refusal}");
    }
}

/// PostgreSQL takes settings in other shapes too: `-F` is fsync off, and
/// `--name=value` and `-cname=value` set any setting. The pin reads only
/// `-c name=value`, so every other shape is refused rather than skipped.
#[test]
fn an_argument_the_pin_cannot_read_by_name_is_refused() {
    for extra in [
        ["-F", "-F"],
        ["--fsync=off", "--fsync=off"],
        ["-cfsync=off", "-cfsync=off"],
        ["-c", "fsync"],
    ] {
        assert!(
            settings_in(&manifest_with(&extra)).is_err(),
            "{extra:?} must be refused"
        );
    }
}
