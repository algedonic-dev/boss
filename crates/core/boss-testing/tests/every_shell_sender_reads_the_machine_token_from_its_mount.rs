//! Every shell sender of the estate machine token takes it through ONE
//! reader, `machine_token_header` in `infra/lib/secret-header.sh` (design
//! 6805c764 car 4; backlog 1876bbdb INFO-7; the car-4 mount checklist on
//! backlog 2710c8fc).
//!
//! THE DEFECT. Until car 4 fifteen scripts under infra/ stamped
//! `$BOSS_MACHINE_TOKEN` — an env var an optional `secretKeyRef` would
//! have set — onto whatever `$BASE` they were handed. Two things were
//! wrong with that the day the token is mounted:
//!
//!   * the env var is not where the token lives. Car 4 mounts the
//!     `boss-machine-token` Secret as a DIRECTORY and deletes the
//!     `secretKeyRef`, so every one of those scripts would have gone out
//!     unstamped in silence (design choice 2: an env var is fixed at
//!     process start, and two sources for one fact drift, CLAUDE.md §9a);
//!   * nothing checked the host. A hand run with the token set and
//!     `BOSS_JOBS_URL` pointed at the public edge sent it there — the
//!     shell half of backlog 2ee29275 F1, which boss-core's clients had
//!     already closed.
//!
//! So this pin reads every file under infra/ and refuses, naming the
//! file and line:
//!
//!   1. any live (non-comment) line that reads the env var
//!      `BOSS_MACHINE_TOKEN` itself — `BOSS_MACHINE_TOKEN_DIR` and
//!      `BOSS_MACHINE_TOKEN_HOSTS` are other names and pass;
//!   2. any `secret_header` call that writes the `x-boss-machine-token`
//!      header by hand, outside the lib: the header comes from
//!      `machine_token_header`, which reads the mount and checks the host;
//!   3. any live line that names `machine_gate_value_header` — the lib's
//!      second writer, for a value its CALLER holds — outside the roster
//!      of one below (adversarial review 4d39f4dc, B1). Without this the
//!      first two rules had a door around them: read the estate token by
//!      hand, hand it to that function, and this pin stayed green. The
//!      roster excuses rule 3 alone; rules 1 and 2 still judge that file.
//!
//! EXEMPT, each for its reason: the lib itself (the one place the header
//! is written), applied migrations (history, never edited —
//! migrations-append-only.sh), and the argv lint, whose fixtures plant
//! the old shapes on purpose to prove it still catches them.
//!
//! tree-wide pin — it scans every file under infra/, which no
//! changed-file map attributes to this crate, so every scoped gate runs
//! it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

const EXEMPT: &[(&str, &str)] = &[
    (
        "infra/lib/secret-header.sh",
        "the one reader: the header is written here and nowhere else",
    ),
    (
        "infra/lint/a-secret-header-rides-in-a-file.sh",
        "its fixtures plant the old argv shapes, env var and all, to prove the lint catches them",
    ),
];
const EXEMPT_DIRS: &[(&str, &str)] = &[(
    "infra/postgres/schema",
    "applied migrations are history (migrations-append-only.sh)",
)];

/// THE ONE CALLER OF `machine_gate_value_header` (adversarial review
/// 4d39f4dc, B1). That function writes the machine-token header for a
/// value its CALLER holds, so it is a door around rule 1 and rule 2 for
/// anyone who may call it: read the estate token by hand, pass it in.
/// A roster of one, each entry with why it cannot read a mount instead.
const VALUE_DOOR_CALLERS: &[(&str, &str)] = &[(
    "infra/forge/probe-reader-deposit.sh",
    "it asks every gate what it makes of the probe reader's `current` slot BEFORE that value \
     is written anywhere a reader could find it, so there is no mount to read: the value is \
     the Secret's, in a variable, and never the estate token (the script refuses any rule \
     that does not declare gate_slots = \"reader\")",
)];
const VALUE_DOOR: &str = "machine_gate_value_header";

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#') || t.starts_with("//")
}

/// Does `line` name the env var `BOSS_MACHINE_TOKEN` itself — not a
/// longer name that begins with it?
fn names_the_env_var(line: &str) -> bool {
    const VAR: &str = "BOSS_MACHINE_TOKEN";
    line.match_indices(VAR).any(|(i, _)| {
        let after = line[i + VAR.len()..].chars().next();
        let before = line[..i].chars().last();
        !matches!(after, Some(c) if c == '_' || c.is_ascii_alphanumeric())
            && !matches!(before, Some(c) if c == '_' || c.is_ascii_alphanumeric())
    })
}

fn writes_the_header_by_hand(line: &str) -> bool {
    line.contains("secret_header ") && line.to_ascii_lowercase().contains("x-boss-machine-token:")
}

/// Does `line` name the value door at all? Any mention on a live line —
/// a call, an alias, a `command -v` — is the caller's to answer for.
fn calls_the_value_door(line: &str) -> bool {
    line.contains(VALUE_DOOR)
}

fn negative_fixture_data(path: &str, line: &str) -> bool {
    // Exactly two inert hostile inputs in the owning worker tests.
    // The file still passes through the whole scan: actual env reads
    // and manual headers, including later additions HERE, are refused.
    path == "infra/dev/dev-build_test.py"
        && matches!(
            line.trim(),
            r#"{"name": "BOSS_MACHINE_TOKEN", "value": "fake"}),"#
                | r#"for entry in ("BASH_ENV=/candidate", "PATH=/candidate", "BOSS_ACTOR=admin", "BOSS_MACHINE_TOKEN=secret", "BOSS_JOBS_URL=bad\0value"):"#
        )
}

#[test]
fn fake_worker_inputs_do_not_hide_a_real_token_use_in_the_same_file() {
    let path = "infra/dev/dev-build_test.py";
    let data = r#"{"name": "BOSS_MACHINE_TOKEN", "value": "fake"}),"#;
    assert!(negative_fixture_data(path, data));
    assert!(!negative_fixture_data("infra/dev/real-sender.py", data));
    for live in [
        r#"token = os.environ["BOSS_MACHINE_TOKEN"]"#,
        r#"secret_header MT_HDR "x-boss-machine-token: $BOSS_MACHINE_TOKEN""#,
    ] {
        assert!(!negative_fixture_data(path, live));
        assert!(names_the_env_var(live) || writes_the_header_by_hand(live));
    }
    let root = boss_testing::scratch_dir("fake-input-real-token-read");
    let target = root.join(path);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let source = std::fs::read_to_string(repo_root().join(path)).unwrap();
    std::fs::write(&target, &source).unwrap();
    assert!(offenders(&root).is_empty());
    std::fs::write(&target, format!("{source}\ntoken = os.environ[\"BOSS_MACHINE_TOKEN\"]\nsecret_header MT_HDR \"x-boss-machine-token: $token\"\n")).unwrap();
    assert_eq!(
        offenders(&root).len(),
        2,
        "real reads and header sends still fail inside the fixture file"
    );
}

fn offenders(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let rel = f.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if EXEMPT.iter().any(|(e, _)| *e == rel)
            || EXEMPT_DIRS
                .iter()
                .any(|(d, _)| rel.starts_with(&format!("{d}/")))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue; // not text
        };
        for (n, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if negative_fixture_data(&rel, line) {
                continue;
            }
            if names_the_env_var(line) {
                out.push(format!(
                    "{rel}:{}: reads the env var: {}",
                    n + 1,
                    line.trim()
                ));
            } else if writes_the_header_by_hand(line) {
                out.push(format!(
                    "{rel}:{}: writes the header by hand: {}",
                    n + 1,
                    line.trim()
                ));
            } else if calls_the_value_door(line)
                && !VALUE_DOOR_CALLERS.iter().any(|(f, _)| *f == rel)
            {
                out.push(format!(
                    "{rel}:{}: calls the value door, which only its rostered caller may: {}",
                    n + 1,
                    line.trim()
                ));
            }
        }
    }
    out
}

#[test]
fn every_shell_sender_reads_the_machine_token_from_its_mount() {
    let found = offenders(&repo_root());
    assert!(
        found.is_empty(),
        "these lines take the machine token some way other than \
         machine_token_header (infra/lib/secret-header.sh), which reads the `current` slot \
         of the mounted Secret and stamps only an estate host — the env var is no longer \
         set anywhere, so a script reading it goes out unstamped in silence:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_matchers_see_what_they_name_and_nothing_longer() {
    assert!(names_the_env_var(
        "secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+\"x-boss-machine-token: $BOSS_MACHINE_TOKEN\"}"
    ));
    assert!(names_the_env_var("    - name: BOSS_MACHINE_TOKEN"));
    assert!(names_the_env_var(
        "if [ -n \"${BOSS_MACHINE_TOKEN:-}\" ]; then"
    ));
    assert!(!names_the_env_var(
        "- {name: BOSS_MACHINE_TOKEN_HOSTS, value: x}"
    ));
    assert!(!names_the_env_var("export BOSS_MACHINE_TOKEN_DIR"));
    assert!(!names_the_env_var("MY_BOSS_MACHINE_TOKEN=1"));
    assert!(writes_the_header_by_hand(
        "secret_header MT_HDR \"x-boss-machine-token: $T\""
    ));
    assert!(!writes_the_header_by_hand(
        "machine_token_header MT_HDR \"$BASE\""
    ));
    assert!(is_comment("  # BOSS_MACHINE_TOKEN was read here"));
    assert!(calls_the_value_door(
        "machine_gate_value_header MT_HDR \"$BASE\" \"$TOK\""
    ));
    assert!(calls_the_value_door(
        "if ! machine_gate_value_header H \"$U\" \"$V\" 2>\"$E\"; then"
    ));
    assert!(calls_the_value_door("alias mgvh=machine_gate_value_header"));
    assert!(!calls_the_value_door(
        "machine_token_header MT_HDR \"$BASE\""
    ));
}

/// THE VALUE DOOR HAS ONE CALLER (adversarial review 4d39f4dc, B1).
/// The reviewer's fixture, handed to the scan and never executed: a
/// script under infra/forge that reads the estate token's slot by hand,
/// and one that takes it from a variable of another name, each passing
/// it to `machine_gate_value_header`. Before the third rule both went
/// through this pin green, so a second sender could take the estate
/// token from anywhere. The same call in the one rostered file passes,
/// and a comment that names the function is not a call.
#[test]
fn a_second_caller_of_the_value_door_is_refused() {
    let root = boss_testing::scratch_dir("value-door-second-caller");
    let forge = root.join("infra/forge");
    std::fs::create_dir_all(&forge).unwrap();
    // Never executed: a data file the scan reads, written as one.
    boss_testing::write_file(
        &forge.join("hand-read-sender.sh"),
        "# machine_gate_value_header is the lib's, and this comment is not a call\n\
         TOK=\"$(cat /etc/boss/machine-token/current)\"\n\
         machine_gate_value_header MT_HDR \"$BASE\" \"$TOK\"\n\
         if ! machine_gate_value_header OTHER_HDR \"$BASE\" \"$SOME_OTHER_NAME\"; then exit 1; fi\n",
    );
    let found = offenders(&root);
    assert_eq!(
        found.len(),
        2,
        "each call outside the roster is an offender, and the comment is not: {found:?}"
    );
    assert!(
        found
            .iter()
            .all(|l| l.starts_with("infra/forge/hand-read-sender.sh:")
                && l.contains("calls the value door")),
        "{found:?}"
    );
    // The rostered caller, with the very same line, passes — and only for
    // this rule: a hand-written header in it is still refused.
    let (rostered, _) = VALUE_DOOR_CALLERS[0];
    boss_testing::write_file(
        &root.join(rostered),
        "machine_gate_value_header READER_HDR \"$SCHEME://$HOST\" \"$V\"\n",
    );
    std::fs::remove_file(forge.join("hand-read-sender.sh")).unwrap();
    assert!(offenders(&root).is_empty(), "{:?}", offenders(&root));
    boss_testing::write_file(
        &root.join(rostered),
        "secret_header READER_HDR \"x-boss-machine-token: $V\"\ntoken=\"$BOSS_MACHINE_TOKEN\"\n",
    );
    assert_eq!(
        offenders(&root).len(),
        2,
        "the roster excuses the value door alone, never the two older rules"
    );
    assert_eq!(
        VALUE_DOOR_CALLERS.len(),
        1,
        "the value door has ONE caller; a second is a decision made here, with its reason"
    );
}

/// Every exemption names a file that exists — a stale exemption is a
/// hole the next file could walk through.
#[test]
fn every_exemption_names_a_file_that_exists() {
    let root = repo_root();
    for (f, why) in EXEMPT {
        assert!(root.join(f).is_file(), "{f} ({why}) is gone — drop it");
    }
    for (d, why) in EXEMPT_DIRS {
        assert!(root.join(d).is_dir(), "{d} ({why}) is gone — drop it");
    }
    for (f, why) in VALUE_DOOR_CALLERS {
        assert!(root.join(f).is_file(), "{f} ({why}) is gone — drop it");
        let text = std::fs::read_to_string(root.join(f)).unwrap();
        assert!(
            text.lines()
                .any(|l| !is_comment(l) && l.contains(VALUE_DOOR)),
            "{f} no longer calls {VALUE_DOOR} — drop it from the roster"
        );
    }
}
