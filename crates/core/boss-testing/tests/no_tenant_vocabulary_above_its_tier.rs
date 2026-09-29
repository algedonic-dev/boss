//! `infra/lint/no-tenant-vocabulary-above-its-tier.sh` is RUN, not read
//! — against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog be39298f). The tier rule — no tenant-specific
//! assumptions in BOSS core (CLAUDE.md §10) — was enforced only as a
//! Cargo dependency audit. Nothing counted tenant VOCABULARY, so on
//! 2026-09-16 a grep for the brewery's words outside the brewery found
//! 362 files across every tier above it: kegs in core registries, excise
//! in module ledgers, the brewery simulator in an orchestrator crate.
//! Each concentration is its own car; this lint is the SENSOR with a
//! ratchet that stops the number rising while those cars land, and holds
//! it at zero afterwards.
//!
//! THE SHAPE. The word list is the tenant's own (`examples/<tenant>/
//! VOCABULARY`), the count is derived from the tree on every run, and the
//! baseline (`infra/lint/tenant-vocabulary.baseline`, one `<count>\t<path>`
//! line per FILE that still carries a word) may only be REWRITTEN DOWN,
//! in the same car that lowers the count (CLAUDE.md §9a). So there are
//! three verdicts on a file, and this file owns all three: above its line
//! (or any word in a file with no line) is refused naming the file; equal
//! is clean; below is refused too, telling the author to lower the line
//! or delete it — which is also what stops anyone raising one.
//!
//! WHY PER FILE (backlog e889cfa4, 2026-09-27). The baseline was one
//! line per TIER, so every car that removed a demo word from
//! `apps/web/src` rewrote the same integer: three cars did in one
//! afternoon, and the first to land left the others conflicting with
//! main on that line. Per file, two cars lowering two files touch two
//! lines — and `two_files_lowering_their_counts_merge_without_a_conflict`
//! runs git's own three-way merge to prove it rather than assert it.
//!
//! Fixtures live under `boss_testing::scratch`, which carries the uid
//! and the pid. The fixture vocabulary is invented (`wibble`, `flurble`)
//! so nothing here spells a real tenant's word: a test that carried one
//! would be one more leak for the lint it proves to count, were test
//! files not excluded — and the exclusion is one of the properties
//! pinned below, so the fixture must not depend on it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/no-tenant-vocabulary-above-its-tier.sh";
const BASELINE: &str = "infra/lint/tenant-vocabulary.baseline";

/// The eight tier roots the lint scans, spelled here so the fixture
/// can create them all: the lint refuses a tree missing one, because a
/// wrong path answers 0 instead of erroring (CLAUDE.md §Doors).
const TIERS: [&str; 8] = [
    "crates/core",
    "crates/modules",
    "crates/orchestrators",
    "apps/web/src",
    "apps/simulator",
    "infra/postgres/schema",
    "infra/platform",
    "infra/cluster",
];

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: the lint itself
/// at the path its own `cd "$(dirname $0)/../.."` resolves from, every
/// tier root, one tenant's vocabulary, and a baseline the test writes.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("tenant-vocabulary-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        for tier in TIERS {
            scratch::create_dir(&root.join(tier));
        }
        let body = std::fs::read_to_string(lint()).expect("read the lint under test");
        scratch::write_exec(&root.join(LINT), &body);
        boss_testing::copy_lint_libs(&root);
        Tree(root)
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
        self
    }

    /// A tenant's word list: `wibble*` is a word-start prefix, `flurble`
    /// a whole word — both spellings the real files use. The tenant is
    /// NAMED for its first term (`examples/wibble`), the way the brewery
    /// is, so the name-as-path and name-as-id exemptions can be pinned.
    fn vocabulary(&self) -> &Tree {
        self.file(
            "examples/wibble/VOCABULARY",
            "# why: the fixture tenant's words\nwibble*\nflurble\n",
        )
    }

    /// The same word list plus one DISCLAIMED PHRASE (`!<phrase>`): a
    /// literal that carries a term but names something outside this
    /// tenant, the way `brew install` carries `brew*` and names
    /// Homebrew (backlog e9423392).
    fn vocabulary_disclaiming(&self, phrase: &str) -> &Tree {
        self.file(
            "examples/wibble/VOCABULARY",
            &format!("# why: the fixture tenant's words\nwibble*\nflurble\n!{phrase}\n"),
        )
    }

    /// A baseline naming exactly these files, one `<count>\t<path>` line
    /// each — the shape the real file has. No entries is a baseline that
    /// says every tier is clean.
    fn baseline(&self, entries: &[(usize, &str)]) -> &Tree {
        let mut body = String::from("# fixture baseline\n");
        for (count, path) in entries {
            body.push_str(&format!("{count}\t{path}\n"));
        }
        self.file(BASELINE, &body)
    }

    fn run(&self) -> Output {
        self.run_with(&[])
    }

    fn run_with(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }

    /// The lint's own measurement of this tree, as a baseline body.
    fn measure(&self) -> String {
        let out = self.run_with(&["--measure"]);
        assert!(
            out.status.success(),
            "--measure must exit 0 on a tree it can read; got {:?}:\n{}",
            out.status.code(),
            text(&out)
        );
        String::from_utf8(out.stdout).expect("--measure prints UTF-8")
    }
}

/// The file every single-file case below writes its words into.
const CORE_FILE: &str = "crates/core/boss-thing/src/lib.rs";

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// ABOVE its line — the leak grew — is refused, and the refusal names
/// the tier, the file, and both numbers, so nobody re-derives them
/// (CLAUDE.md §Diagnosis).
#[test]
fn a_file_above_its_baseline_is_refused_naming_the_file() {
    let tree = Tree::new("above");
    tree.vocabulary().baseline(&[(1, CORE_FILE)]).file(
        CORE_FILE,
        "// a Wibbler and a wibble: two hits\nfn flurble() {}\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "three hits over a baseline of one must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "crates/core",
        "crates/core/boss-thing/src/lib.rs",
        "3",
        "baseline",
    ] {
        assert!(
            msg.contains(expect),
            "the refusal must name {expect:?}:\n{msg}"
        );
    }
}

/// EQUAL to the baseline is clean — and the count is case-insensitive,
/// occurrence-based (two on one line are two), and whole-word for a bare
/// term: `flurbles` is not `flurble`, while `Wibbling` IS `wibble*`.
#[test]
fn a_file_at_its_baseline_is_clean() {
    let tree = Tree::new("equal");
    tree.vocabulary().baseline(&[(3, CORE_FILE)]).file(
        CORE_FILE,
        "// Wibbling wibble — two; flurbles is not flurble, so one more\nfn flurble() {}\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a count equal to its baseline must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("clean"),
        "a pass must say so:\n{}",
        text(&out)
    );
}

/// BELOW the baseline is refused too, and the refusal is an
/// instruction: lower the baseline in this car. This is the whole
/// ratchet — a baseline that may sit above the count is a baseline
/// anyone can raise, and the number would stop meaning anything.
#[test]
fn a_file_below_its_baseline_is_told_to_lower_it() {
    let tree = Tree::new("below");
    tree.vocabulary()
        .baseline(&[(5, CORE_FILE)])
        .file(CORE_FILE, "fn wibble() {}\n");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a baseline above the count must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("lower")
            && msg.contains(CORE_FILE)
            && msg.contains("baseline 5")
            && msg.contains("set its line to 1"),
        "the refusal must tell the author to LOWER the file's line, naming \
         the file and both numbers:\n{msg}"
    );
}

/// A tenant's NAME used as a path or as an id is not vocabulary
/// leaking: the product must be able to say which tenant an instance
/// runs. The two forms are `examples/<tenant>` and
/// `tenant_id = "<tenant>"` — the shapes infra/cluster/instances.toml
/// and the tenant-contract table landed with on 2026-09-16 — and only
/// those: the same name as a bare word on the same line still counts.
#[test]
fn a_tenant_name_as_a_path_or_an_id_is_not_vocabulary() {
    let tree = Tree::new("name-as-path-or-id");
    tree.vocabulary().baseline(&[(1, CORE_FILE)]).file(
        CORE_FILE,
        "\
// tenant =\"examples/wibble/seeds/tenant.toml\" names a path, not a word
// tenant_id = \"wibble\" names an id; TENANT_ID = \"Wibble\" too
// /opt/boss/examples/wibble/data is the same path form, deeper
// but a plain wibble here is the one hit that counts
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "three name-as-path/id forms and one word must count exactly one; \
         got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );

    // And the exemption does not reach past its two forms: the name as a
    // bare word, or as a different key's value, still counts.
    let tree = Tree::new("name-as-word");
    tree.vocabulary().baseline(&[]).file(
        CORE_FILE,
        "// kind =\"wibble\" is not the tenant_id key, and Wibble alone is a word\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "the name outside the two forms must still count:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("crates/core/boss-thing/src/lib.rs"),
        "and be named:\n{}",
        text(&out)
    );
}

/// A migration's DELETE names what it removes, and is not counted
/// (backlog b5f21e82, 2026-09-18). The rows the historical migrations
/// inserted under the demo tenant's rule names are the leak in the
/// running system, and the one migration that deletes them has to spell
/// those names — inside a `DELETE FROM … ;` statement, where a word can
/// only leave the product, never arrive. Every file in this tier is
/// applied history and cannot be edited, so without this form the
/// tier's count could never fall to zero once that migration landed.
/// The exemption reaches exactly the statement: the same word in an
/// INSERT beside it, or in a comment above it, still counts.
#[test]
fn a_word_inside_a_migrations_delete_is_the_leak_leaving() {
    let tree = Tree::new("delete-statement");
    tree.vocabulary().baseline(&[]).file(
        "infra/postgres/schema/20260918000000-residue-goes.sql",
        "\
-- a header that names no term
DELETE FROM some_rules d
 WHERE d.source IS NULL
   AND d.name IN (
    'spawn-wibble-return-on-delivery',
    'flurble-on-close'
   );
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "two terms inside a DELETE statement must count zero; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );

    // And only the statement: an INSERT of the same name, or the word in
    // a comment outside the DELETE, is still the leak arriving.
    let tree = Tree::new("delete-statement-bounds");
    tree.vocabulary().baseline(&[]).file(
        "infra/postgres/schema/20260918000001-not-only-a-delete.sql",
        "\
-- the wibble reactor, named in prose above the statement: one hit
DELETE FROM some_rules WHERE name = 'spawn-wibble-return';
INSERT INTO some_rules (name) VALUES ('spawn-wibble-return');
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a term in the comment and one in the INSERT must still count:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("     2  infra/postgres/schema/20260918000001-not-only-a-delete.sql"),
        "exactly two of the three occurrences count, and the file is named:\n{}",
        text(&out)
    );
}

/// Test files are not counted. `#[cfg(test)]` blocks inside a source
/// file ARE — the lint says so in its header — so the exclusion is by
/// path only, and this pins the shapes it recognises.
#[test]
fn test_files_are_not_counted() {
    let tree = Tree::new("tests");
    tree.vocabulary()
        .baseline(&[])
        .file("crates/core/boss-thing/tests/a.rs", "fn wibble() {}\n")
        .file("crates/core/boss-thing/src/a_test.rs", "fn wibble() {}\n")
        .file("apps/web/src/a/a.test.ts", "const flurble = 1;\n")
        .file("apps/web/src/a/a.spec.ts", "const flurble = 1;\n");
    let out = tree.run();
    assert!(
        out.status.success(),
        "a term in a test file must not count; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// A tree with no tenant vocabulary is a wrong path, not a clean tree:
/// zero words would count zero hits everywhere and certify nothing.
#[test]
fn a_tree_with_no_vocabulary_is_refused() {
    let tree = Tree::new("no-vocabulary");
    tree.baseline(&[]);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "no VOCABULARY file must be a refusal, never clean:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("VOCABULARY"),
        "the refusal must name what is missing:\n{}",
        text(&out)
    );
}

/// The real tree passes: every file's count equals its baseline line.
/// This is the ratchet on the repository itself — a car that adds a
/// brewery word to core, or removes one without lowering the baseline,
/// reds here before it reds a train.
#[test]
fn the_repository_itself_is_at_its_baseline() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint against the repository");
    assert!(
        out.status.success(),
        "every file must sit exactly at its baseline; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// A word in a file the baseline does not name is refused, naming that
/// file — its baseline is 0. And it is refused even when the TIER's total
/// is unchanged: a word moved from one file to another kept the old
/// per-tier line equal and passed, while here both files are named, the
/// one that gained and the one whose line must fall.
#[test]
fn a_word_in_a_file_the_baseline_does_not_name_is_refused() {
    let listed = "apps/web/src/a/Listed.svelte";
    let unlisted = "apps/web/src/b/Unlisted.svelte";
    let tree = Tree::new("unlisted-file");
    tree.vocabulary()
        .baseline(&[(2, listed)])
        .file(listed, "<p>one wibble</p>\n")
        .file(unlisted, "<p>one flurble</p>\n");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a word moved into an unlisted file must be refused even though the \
         tier's total is still 2:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains(unlisted) && msg.contains("not in the baseline"),
        "the refusal must name the unlisted file as not in the baseline:\n{msg}"
    );
    assert!(
        msg.contains(listed) && msg.contains("set its line to 1"),
        "and name the listed file's line to lower:\n{msg}"
    );
}

/// A line for a file that no longer carries a word is refused until the
/// line is deleted, in the same car — a stale line is a number nobody is
/// ratcheting. So is a line of 0, a line naming a path under no tier the
/// lint scans, a repeated path, and a line in the old per-tier shape:
/// each would read as covering something while covering nothing.
#[test]
fn a_stale_or_malformed_baseline_line_is_refused() {
    let tree = Tree::new("stale-line");
    tree.vocabulary()
        .baseline(&[(2, CORE_FILE)])
        .file(CORE_FILE, "fn clean() {}\n");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a line for a now-clean file must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains(CORE_FILE) && msg.contains("delete its line"),
        "the refusal must name the file and say to delete its line:\n{msg}"
    );

    for (tag, body, expect) in [
        ("zero", format!("0\t{CORE_FILE}\n"), "0"),
        (
            "untiered",
            "1\texamples/wibble/seeds/a.toml\n".to_string(),
            "examples/wibble/seeds/a.toml",
        ),
        (
            "repeated",
            format!("1\t{CORE_FILE}\n1\t{CORE_FILE}\n"),
            "more than once",
        ),
        ("per-tier", "crates/core 1\n".to_string(), "crates/core 1"),
    ] {
        let tree = Tree::new(&format!("malformed-{tag}"));
        tree.vocabulary()
            .file(BASELINE, &body)
            .file(CORE_FILE, "fn wibble() {}\n");
        let out = tree.run();
        assert_eq!(
            out.status.code(),
            Some(1),
            "a {tag} baseline line must be refused:\n{}",
            text(&out)
        );
        let msg = text(&out);
        assert!(
            msg.contains(BASELINE) && msg.contains(expect),
            "the {tag} refusal must name the baseline and {expect:?}:\n{msg}"
        );
    }
}

/// `--measure` prints the baseline the lint accepts: the per-file table,
/// one `<count>\t<path>` entry per file carrying a word, in path order.
/// The repository's baseline was written from it rather than typed.
#[test]
fn measure_prints_the_baseline_the_lint_accepts() {
    let tree = Tree::new("measure");
    tree.vocabulary()
        .baseline(&[])
        .file(CORE_FILE, "fn wibble() {} // flurble\n")
        .file("apps/web/src/a/A.svelte", "<p>wibble</p>\n");
    let measured = tree.measure();
    let entries: Vec<&str> = measured.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        entries,
        ["1\tapps/web/src/a/A.svelte", &format!("2\t{CORE_FILE}")],
        "one entry per file, count then path, sorted by path:\n{measured}"
    );
    tree.file(BASELINE, &measured);
    let out = tree.run();
    assert!(
        out.status.success(),
        "the measured baseline must be clean; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// THE DEFECT THIS SHAPE EXISTS FOR (backlog e889cfa4). Two cars remove
/// words from two NEIGHBOURING files of one tier — the adjacent pair is
/// the hard case, because git's three-way merge refuses two edits to
/// adjacent lines. Each car lowers its own file's line; git merges the
/// two baselines with no conflict, and the assembled tree is clean.
/// Under the per-tier shape both cars rewrote the one `apps/web/src`
/// line and the second always conflicted.
#[test]
fn two_files_lowering_their_counts_merge_without_a_conflict() {
    let a = "apps/web/src/a/A.svelte";
    let b = "apps/web/src/a/B.svelte";
    let c = "apps/web/src/a/C.svelte";
    let two = "<p>wibble flurble</p>\n";
    let one = "<p>wibble</p>\n";

    let state = |tag: &str, b_body: &str, c_body: &str| -> (Tree, String) {
        let tree = Tree::new(tag);
        tree.vocabulary()
            .baseline(&[])
            .file(a, two)
            .file(b, b_body)
            .file(c, c_body);
        let measured = tree.measure();
        (tree, measured)
    };
    let (_base_tree, base) = state("merge-base", two, two);
    let (_ours_tree, ours) = state("merge-ours", one, two);
    let (_theirs_tree, theirs) = state("merge-theirs", two, one);
    assert_ne!(base, ours, "car 1 must change the baseline");
    assert_ne!(base, theirs, "car 2 must change the baseline");

    let dir = scratch::scratch_dir("tenant-vocabulary-merge-files");
    for (name, body) in [("base", &base), ("ours", &ours), ("theirs", &theirs)] {
        scratch::write_file(&dir.join(name), body);
    }
    let merged = Command::new("git")
        .args(["merge-file", "-p", "ours", "base", "theirs"])
        .current_dir(&dir)
        .output()
        .expect("run git merge-file");
    let _ = std::fs::remove_dir_all(&dir);
    let merged_text = String::from_utf8_lossy(&merged.stdout).to_string();
    assert_eq!(
        merged.status.code(),
        Some(0),
        "two cars lowering two neighbouring files must merge with no \
         conflict:\n--- base\n{base}--- ours\n{ours}--- theirs\n{theirs}--- merged\n{merged_text}"
    );

    let (assembled, _) = state("merge-assembled", one, one);
    assembled.file(BASELINE, &merged_text);
    let out = assembled.run();
    assert!(
        out.status.success(),
        "the merged baseline must be clean on the assembled tree; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// A PHRASE the tenant DISCLAIMS is not its vocabulary (backlog
/// e9423392). `brew*` is a word-start prefix that is right in general —
/// brewery, brewhouse, brewing — and wrong for one common literal:
/// `brew install` names Homebrew. A builder writing macOS install steps
/// hit this on 2026-09-20 and worked around it by writing a different
/// command; the next one will not always have as clean a dodge.
///
/// The VOCABULARY file already carried this judgement as PROSE — its
/// "deliberately NOT listed" block, naming tap, batch, hop, grain,
/// barrel, ale. A comment asking the next person to make the same call
/// is not a mechanism (CLAUDE.md §9a), so a `!<phrase>` line makes one
/// class of it executable, subtracted with the same exact weighting the
/// name-as-path and DELETE exemptions already use.
#[test]
fn a_phrase_the_tenant_disclaims_is_not_its_vocabulary() {
    let tree = Tree::new("disclaimed-phrase");
    tree.vocabulary_disclaiming("wibble sprocket")
        .baseline(&[(1, CORE_FILE)])
        .file(
            "crates/core/boss-thing/src/lib.rs",
            "\
// a wibble sprocket is the disclaimed phrase, and a Wibble Sprocket is it too
// but a plain wibble here is the one hit that counts
",
        );
    let out = tree.run();
    assert!(
        out.status.success(),
        "two disclaimed phrases and one word must count exactly one; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );

    // The exemption reaches the phrase and nothing else: the same term
    // in any other company still counts.
    let tree = Tree::new("disclaimed-phrase-is-not-the-term");
    tree.vocabulary_disclaiming("wibble sprocket")
        .baseline(&[])
        .file(
            "crates/core/boss-thing/src/lib.rs",
            "// a wibble widget is not the disclaimed phrase\n",
        );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "the term outside the disclaimed phrase must still count:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("crates/core/boss-thing/src/lib.rs"),
        "and be named:\n{}",
        text(&out)
    );
}

/// A disclaimed phrase that carries no term would exempt nothing — a
/// declaration that reads as covering something and covers nothing, the
/// wrong-path-answers-0 shape (CLAUDE.md §Doors). Refused, naming it.
#[test]
fn a_disclaimed_phrase_carrying_no_term_is_refused() {
    let tree = Tree::new("disclaimed-phrase-without-a-term");
    tree.vocabulary_disclaiming("sprocket widget").baseline(&[]);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a phrase matching no term must be a refusal, never a silent no-op:\n{}",
        text(&out)
    );
    let t = text(&out);
    assert!(
        t.contains("sprocket widget") && t.contains("disclaim"),
        "the refusal must name the phrase and the mechanism it belongs to:\n{t}"
    );
}

/// The brewery's own disclaimer, in the real tree: `brew install` is
/// Homebrew, not this tenant. The declaration lives with the tenant and
/// the lint reads it there, so this pins the one line the repository
/// depends on — without it, the phrase counts again and the next
/// builder writing install steps is refused as this one was.
#[test]
fn the_brewery_disclaims_brew_install() {
    let vocab = repo_root().join("examples/brewery/VOCABULARY");
    let body = std::fs::read_to_string(&vocab).expect("read the brewery vocabulary");
    assert!(
        body.lines().any(|l| l.trim() == "!brew install"),
        "the tenant must disclaim the phrase in its own file:\n{body}"
    );
}
