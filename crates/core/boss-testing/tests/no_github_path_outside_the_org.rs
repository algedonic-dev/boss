//! `infra/lint/no-github-path-outside-the-org.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog d2b7c947, David 2026-09-30: "we have made a
//! fundamental change to our posture and now are working professionally
//! out of the algedonic-dev Github org via the Github App"). Until that
//! day the public mirror's pull request came from a public fork in
//! David's personal account, pushed with a personal access token he
//! minted by hand, and the fork stood declared as an off-site push
//! target with a token slot of its own. Car 1 moved the publish inside
//! the organisation, opened as the App; car 2 retired every other
//! personal-account path. This lint refuses the next one at pre-flight:
//! a GitHub remote, a `gh` slug or head, a REST path, a token slot or a
//! per-account no-reply identity outside the organisation, in any
//! tracked file under infra/, .forgejo/ or .github/.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "each shape still matches and still sorts"; this file owns the
//! VERDICT on a tree — the organisation's paths and reads of public pages
//! are clean and the count says how many files were read; a personal path
//! exits 1 naming file, line and what it named, in each scanned
//! directory; a file outside them (a test fixture, a person's record) is
//! not the lint's business; the one history allowance excuses exactly its
//! file and goes stale loudly when that file goes; and a tree it cannot
//! read exits 3 rather than certifying it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/no-github-path-outside-the-org.sh";
/// The one applied migration the lint excuses, as history.
const HISTORY: &str = "infra/postgres/schema/202609081230-the-dauld-github-token-is-declared.sql";

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked file must not count),
/// the lint at the path its own `cd "$(dirname $0)/../.."` resolves from,
/// its helpers, and the history file its allowance names — copied from
/// the tree, so the allowance has the finding it must excuse.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("no-github-path-outside-the-org-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        let history = std::fs::read_to_string(repo_root().join(HISTORY))
            .unwrap_or_else(|e| panic!("read {HISTORY}: {e}"));
        scratch::create_dir(root.join(HISTORY).parent().unwrap());
        scratch::write_file(&root.join(HISTORY), &history);
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "."]);
        Tree(root)
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
        git(&self.0, &["add", rel]);
        self
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// How many helpers `copy_lint_libs` carries into a tree — the .sh files
/// under infra/ it copies, counted, never typed.
fn lint_lib_count() -> usize {
    boss_testing::LINT_LIB_DIRS
        .iter()
        .map(|dir| {
            std::fs::read_dir(repo_root().join(dir))
                .unwrap_or_else(|e| panic!("{dir}: {e}"))
                .filter(|e| {
                    e.as_ref()
                        .ok()
                        .is_some_and(|e| e.path().extension().is_some_and(|x| x == "sh"))
                })
                .count()
        })
        .sum()
}

/// The shapes prove themselves on every invocation and SAY so.
#[test]
fn the_lint_proves_itself_on_every_invocation() {
    let out = Command::new("bash")
        .arg(lint())
        .arg("--self-test")
        .output()
        .expect("run the lint's self-test");
    assert!(
        out.status.success(),
        "the lint's --self-test must pass:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("self-test ok"), "{}", text(&out));
}

/// BEHAVIOUR 1 — the organisation's remotes, slugs, heads, REST paths,
/// token slots and bot identity are clean, as are reads of someone else's
/// public release or page and a value built at run time; the count is the
/// files READ.
#[test]
fn organisation_paths_and_public_reads_are_clean_and_the_count_is_files_read() {
    let tree = Tree::new("clean");
    tree.file(
        "infra/forge/offsite-push.json",
        "{\"targets\": [{\"remote\": \"https://github.com/algedonic-dev/boss-dr.git\", \"token_file\": \"/etc/boss-publish/github-dr.token\"}]}\n",
    );
    tree.file(
        "infra/forge/publish.sh",
        "#!/usr/bin/env bash\n\
         MIRROR=https://github.com/Algedonic-Dev/boss\n\
         gh pr list --repo algedonic-dev/boss --head algedonic-dev:publish/x\n\
         gh pr create --repo \"$SLUG\" --head \"$MIRROR_OWNER:$BRANCH\"\n\
         curl https://api.github.com/repos/algedonic-dev/boss/pulls\n\
         PUBLISH_SLOT=/etc/boss-publish/github-app/algedonic-dev-publish.token\n\
         SLOT=/etc/boss-publish/github-app/$owner.token\n\
         git clone git@github.com:algedonic-dev/boss.git\n",
    );
    tree.file(
        "infra/nats/setup.sh",
        "#!/usr/bin/env bash\ncurl -L https://github.com/nats-io/nats-server/releases/download/v2/nats.tgz\n# see https://github.com/ossf/scorecard#checks and https://github.com/siderolabs/talos/blob/main/README.md\n# https://github.com/settings/apps\n",
    );
    tree.file(
        ".forgejo/workflows/ci.yml",
        "env:\n  GIT_AUTHOR_EMAIL: 41898282+github-actions[bot]@users.noreply.github.com\n",
    );
    tree.file(
        ".github/ISSUE_TEMPLATE/config.yml",
        "contact_links:\n  - url: https://github.com/algedonic-dev/boss/discussions\n",
    );
    // Review b0607d1d, B2: curl's --head is a probe, not a pull request
    // head, and a bare link to someone's project is a page to read, as the
    // header promises — with the organisation's spellings of B1's shapes.
    tree.file(
        "infra/forge/review-clean.sh",
        "#!/usr/bin/env bash\n\
         curl -sS --head https://example.org/x\n\
         curl -sS -o /dev/null --head http://example.org:3000/\n\
         # the GitHub CLI, https://github.com/cli/cli, opens it; https://github.com/orgs/someone/people\n\
         git push HTTPS://GitHub.com/algedonic-dev/boss.git main\n\
         git push https://x-access-token:$T@github.com/algedonic-dev/boss.git\n\
         git push ssh://github.com/algedonic-dev/boss.git\n\
         gh pr list -R algedonic-dev/boss; GH_REPO=algedonic-dev/boss gh pr list\n\
         gh api repos/algedonic-dev/boss/pulls; gh api /orgs/algedonic-dev/installation\n\
         cp -R some/dir elsewhere; rsync -r a/b c\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "the organisation's paths and reads of public pages must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let said = text(&out);
    // Six fixtures and the history file, plus the lint and every helper
    // — which live under infra/ themselves and are read like any other.
    let want = format!("scanned {} file(s)", 6 + 1 + 1 + lint_lib_count());
    assert!(
        said.contains(&want),
        "the count is every file under the scanned directories read ({want}):\n{said}"
    );
    assert!(said.contains("is algedonic-dev's"), "{said}");
}

/// BEHAVIOUR 2 — the shapes car 2 removed, each refused by file and line
/// and named: a personal remote and its token slot (the off-site
/// declaration's fork target), an ssh remote, a `gh` slug and head outside
/// the organisation, a REST path, a lookalike owner that only BEGINS with
/// the organisation's name, and a person's no-reply commit identity — in
/// each scanned directory. Then review b0607d1d's bypasses (B1), each one
/// line: the host in another case, a token before the host, ssh with no
/// user, gh's `-R` and `GH_REPO`, `gh api` with no host, a remote ending
/// in `#` or `/pull`, and a `..` segment (N1).
#[test]
fn a_personal_github_path_is_refused_by_file_line_and_what_it_names() {
    let tree = Tree::new("personal");
    tree.file(
        "infra/forge/offsite-push.json",
        "{\"targets\": [\n{\"remote\": \"https://github.com/someone/boss-mirror.git\",\n\"token_file\": \"/etc/boss-publish/github.token\"}]}\n",
    );
    tree.file(
        "infra/forge/publish.sh",
        "#!/usr/bin/env bash\n\
         git push git@github.com:someone/boss-fork publish/x\n\
         gh pr create --repo algedonic-dev/boss --head someone:publish/x\n\
         curl https://api.github.com/repos/someone/boss\n\
         git push https://github.com/algedonic-dev-x/boss.git\n",
    );
    tree.file(
        "infra/forge/review.sh",
        "#!/usr/bin/env bash\n\
         git push https://GitHub.com/someone/boss-mirror.git main\n\
         git push HTTPS://github.com/someone/boss-mirror.git main\n\
         git push https://x-access-token:$T@github.com/someone/boss-mirror.git\n\
         url = https://someone:${TOKEN}@github.com/someone/boss-mirror.git\n\
         git push ssh://github.com/someone/boss-mirror.git\n\
         gh pr create -R someone/boss\n\
         GH_REPO=someone/boss gh pr list\n\
         gh api repos/someone/boss-mirror/pulls\n\
         gh api /repos/someone/boss\n\
         curl -X PUT https://github.com/someone/boss-mirror.git#\n\
         git push https://github.com/someone/boss-mirror/pull main\n\
         git push https://github.com/algedonic-dev/../cli/cli.git\n",
    );
    tree.file(
        ".forgejo/workflows/ci.yml",
        "env:\n  GIT_AUTHOR_EMAIL: 1234+someone@users.noreply.github.com\n",
    );
    tree.file(
        ".github/workflows/mirror.yml",
        "steps:\n  - run: gh repo view --repo someone/boss\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a personal GitHub path is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    for (site, names) in [
        ("infra/forge/offsite-push.json:2:", "owner someone"),
        (
            "infra/forge/offsite-push.json:3:",
            "/etc/boss-publish/github.token",
        ),
        ("infra/forge/publish.sh:2:", "owner someone"),
        ("infra/forge/publish.sh:3:", "someone:"),
        ("infra/forge/publish.sh:4:", "repos/someone"),
        ("infra/forge/publish.sh:5:", "owner algedonic-dev-x"),
        ("infra/forge/review.sh:2:", "owner someone"),
        ("infra/forge/review.sh:3:", "owner someone"),
        ("infra/forge/review.sh:4:", "owner someone"),
        ("infra/forge/review.sh:5:", "owner someone"),
        ("infra/forge/review.sh:6:", "owner someone"),
        ("infra/forge/review.sh:7:", "-R someone/"),
        ("infra/forge/review.sh:8:", "GH_REPO=someone/"),
        ("infra/forge/review.sh:9:", "repos/someone"),
        ("infra/forge/review.sh:10:", "repos/someone"),
        ("infra/forge/review.sh:11:", "owner someone"),
        ("infra/forge/review.sh:12:", "owner someone"),
        ("infra/forge/review.sh:13:", "dot segment"),
        (".forgejo/workflows/ci.yml:2:", "no-reply identity"),
        (".github/workflows/mirror.yml:2:", "--repo someone/"),
    ] {
        assert!(
            said.lines().any(|l| l.contains(site) && l.contains(names)),
            "{site} is named with what it names ({names}):\n{said}"
        );
    }
    assert!(
        !said
            .lines()
            .any(|l| l.contains("publish.sh:3:") && l.contains("--repo")),
        "the organisation's slug on the same line is not a finding:\n{said}"
    );
}

/// BEHAVIOUR 2b — review 14cd7db9's narrower spellings (backlog 6bfb8f72),
/// each refused by file and line: a doubled slash after the host (R1), a
/// percent-encoded owner (R2), `gh api` behind a flag or a `gh_*` wrapper
/// (R4), a positional `gh repo clone` slug (R5), a URL declared under a
/// key that is neither remote nor url and a push behind git's own
/// `--git-dir` (R6), and a push split across a continuation (R3), named
/// at its first line. Its two false refusals pass: a `cp -R a/b` beside a
/// comment that mentions gh, and the organisation's URL with a port.
#[test]
fn review_14cd7db9s_narrower_spellings_are_refused_and_its_false_refusals_pass() {
    let tree = Tree::new("narrower");
    tree.file(
        "infra/forge/narrower.sh",
        "#!/usr/bin/env bash\n\
         git ls-remote https://github.com//someone/boss-mirror.git\n\
         git push https://github.com/%73omeone/boss-mirror.git main\n\
         gh api -i repos/someone/boss/pulls\n\
         gh_t api \"repos/someone/boss/pulls/1\"\n\
         gh repo clone someone/boss\n\
         git --git-dir=/srv/x push https://github.com/someone/boss-mirror main\n\
         git push \\\n  https://github.com/someone/boss-mirror main\n",
    );
    tree.file(
        "infra/forge/targets.json",
        "{\n\"target\": \"https://github.com/someone/boss-mirror\",\n\"ok\": \"https://github.com/algedonic-dev/boss\"\n}\n",
    );
    tree.file(
        "infra/forge/clean.sh",
        "#!/usr/bin/env bash\n\
         # hand it to gh pr create later; cp -R some/dir elsewhere\n\
         git clone https://github.com:443/algedonic-dev/boss.git\n\
         gh api -i repos/algedonic-dev/boss/pulls\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "each narrower spelling is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    for (site, names) in [
        ("infra/forge/narrower.sh:2:", "owner someone"),
        ("infra/forge/narrower.sh:3:", "owner someone"),
        ("infra/forge/narrower.sh:4:", "repos/someone"),
        ("infra/forge/narrower.sh:5:", "repos/someone"),
        ("infra/forge/narrower.sh:6:", "someone/"),
        ("infra/forge/narrower.sh:7:", "owner someone"),
        ("infra/forge/narrower.sh:8:", "owner someone"),
        ("infra/forge/targets.json:2:", "owner someone"),
    ] {
        assert!(
            said.lines().any(|l| l.contains(site) && l.contains(names)),
            "{site} is named with what it names ({names}):\n{said}"
        );
    }
    for clean in [
        "narrower.sh:9:",
        "targets.json:3:",
        "clean.sh:2:",
        "clean.sh:3:",
        "clean.sh:4:",
    ] {
        assert!(
            !said.lines().any(|l| l.contains(clean)),
            "{clean} names no path outside the organisation:\n{said}"
        );
    }
}

/// BEHAVIOUR 2c — review d07c5c57's rarer spellings (backlog ef646681),
/// each refused by file and line: a GraphQL owner string (M1), in
/// `repository(owner:…)` and as a graphql command's `owner` variable; gh's
/// `-R` glued to its value (M2); a quoted URL alone on an array line under
/// a key opened above it (M3). Its three false refusals pass: a `gh repo
/// clone` destination directory (F1), declared third-party metadata under
/// a docs-shaped key (F2), and `-D repos/cache` (F3) — while a key that
/// names a url, and a clone's first positional, stay refused.
#[test]
fn review_d07c5c57s_rarer_spellings_are_refused_and_its_false_refusals_pass() {
    let tree = Tree::new("rarer");
    tree.file(
        "infra/forge/rarer.sh",
        "#!/usr/bin/env bash\n\
         gh api graphql -f query='{ repository(owner:\"someone\", name:\"boss\") { id } }'\n\
         gh api graphql -F owner=someone -f query=@q.graphql\n\
         gh pr create -Rsomeone/boss\n\
         gh repo clone someone/boss work/dir\n\
         gh repo clone algedonic-dev/boss work/dir\n\
         gh run download 1 -D repos/cache\n",
    );
    tree.file(
        "infra/forge/mirrors.json",
        "{\n\"mirrors\": [\n  \"https://github.com/someone/boss-mirror\"\n]\n}\n",
    );
    tree.file(
        "infra/forge/chart.yaml",
        "sources:\n  - https://github.com/someone/tool\nhomepage: https://github.com/someone/tool\nhomepage_url: https://github.com/someone/boss-mirror\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "each rarer spelling is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    for (site, names) in [
        ("infra/forge/rarer.sh:2:", "GraphQL owner"),
        ("infra/forge/rarer.sh:3:", "GraphQL owner"),
        ("infra/forge/rarer.sh:4:", "-R someone/"),
        ("infra/forge/rarer.sh:5:", "someone/"),
        ("infra/forge/mirrors.json:3:", "owner someone"),
        ("infra/forge/chart.yaml:4:", "owner someone"),
        // Review ab177632: no key is carried down, so a bare item under
        // `sources:` is judged as a remote — the key goes on its line.
        ("infra/forge/chart.yaml:2:", "owner someone"),
    ] {
        assert!(
            said.lines().any(|l| l.contains(site) && l.contains(names)),
            "{site} is named with what it names ({names}):\n{said}"
        );
    }
    for clean in [
        "rarer.sh:5: a gh slug outside algedonic-dev (gh repo work/",
        "rarer.sh:6:",
        "rarer.sh:7:",
        "mirrors.json:2:",
        "chart.yaml:1:",
        "chart.yaml:3:",
    ] {
        assert!(
            !said.lines().any(|l| l.contains(clean)),
            "{clean} names no path outside the organisation:\n{said}"
        );
    }
}

/// BEHAVIOUR 2d — review e9619c13 of the first fold of 2c: what its
/// relaxations opened, each refused. B1: an assignment is never metadata,
/// whatever its name ends in (`publish_home=`, `export FORK_HOME=`), and a
/// docs word must be the WHOLE key (`mirror_docs:`). B2: no list item
/// inherits a `sources:` key from above (since review ab177632, nothing
/// is carried at all). B3: `gh repo clone -- <slug>`. B4: an empty
/// `--input=` does not swallow the endpoint. B5: a docs-shaped key that
/// names a url (`docs_url:`) is a remote. A docs word qualified as a link
/// (`docs_link:`) stays metadata.
#[test]
fn review_e9619c13s_bypasses_of_the_relaxations_are_refused() {
    let tree = Tree::new("e9619c13");
    tree.file(
        "infra/forge/vars.sh",
        "#!/usr/bin/env bash\n\
         publish_home=https://github.com/someone/boss-mirror\n\
         export FORK_HOME=https://github.com/someone/boss-fork\n\
         gh repo clone -- someone/boss-mirror\n\
         gh api --input= repos/someone/boss-mirror/git/refs\n",
    );
    tree.file(
        "infra/forge/meta.yaml",
        "sources:\n  - https://github.com/someone/tool\n\"publish/remotes\":\n  - https://github.com/someone/boss-mirror\nmirror targets:\n  - https://github.com/someone/boss-mirror\nmirror_docs: https://github.com/someone/boss-mirror\ndocs_url: https://github.com/someone/boss-mirror\ndocs_link: https://github.com/someone/tool\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "each bypass is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    for (site, names) in [
        ("infra/forge/vars.sh:2:", "owner someone"),
        ("infra/forge/vars.sh:3:", "owner someone"),
        ("infra/forge/vars.sh:4:", "someone/"),
        ("infra/forge/vars.sh:5:", "repos/someone"),
        ("infra/forge/meta.yaml:2:", "owner someone"),
        ("infra/forge/meta.yaml:4:", "owner someone"),
        ("infra/forge/meta.yaml:6:", "owner someone"),
        ("infra/forge/meta.yaml:7:", "owner someone"),
        ("infra/forge/meta.yaml:8:", "owner someone"),
    ] {
        assert!(
            said.lines().any(|l| l.contains(site) && l.contains(names)),
            "{site} is named with what it names ({names}):\n{said}"
        );
    }
    assert!(
        !said.lines().any(|l| l.contains("meta.yaml:9:")),
        "meta.yaml:9: is declared project metadata:\n{said}"
    );
}

/// BEHAVIOUR 2e — review 70e0d410 of the second fold: a `:=` assignment
/// is never a mapping (C1); and reviews 70e0d410 (C2) and ab177632 found
/// a key carried down to later list items leaking three ways, so nothing
/// is carried: every bare list item is judged as a remote — after an
/// inline-valued key, a document marker, a dedent, or under `sources:`
/// itself. Same-line metadata (`homepage: <url>`, `- homepage: <url>`)
/// passes. `_uri` is a remote like `_url`.
#[test]
fn review_70e0d410s_carry_and_assignment_leaks_are_refused() {
    let tree = Tree::new("70e0d410");
    tree.file(
        "infra/forge/make.mk",
        "homepage := https://github.com/someone/boss-mirror\n\
         docs:=https://github.com/someone/boss-mirror\n",
    );
    tree.file(
        "infra/forge/carry.yaml",
        "targets:\n\
         \x20 - name: dr\n\
         \x20   homepage: https://github.com/algedonic-dev/boss-dr\n\
         \x20 - https://github.com/someone/boss-mirror\n\
         sources:\n\
         \x20 - https://github.com/someone/tool\n\
         ---\n\
         - https://github.com/someone/boss-mirror\n\
         docs_uri: https://github.com/someone/boss-mirror\n\
         sources: # third-party\n\
         \x20 # upstream\n\
         \x20 - https://github.com/someone/tool\n\
         mirrors:\n\
         \x20 - name: docs-site\n\
         \x20   links:\n\
         \x20     - homepage: https://github.com/someone/tool\n\
         \x20 - https://github.com/someone/boss-mirror\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "each leak is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    for site in [
        "infra/forge/make.mk:1:",
        "infra/forge/make.mk:2:",
        "infra/forge/carry.yaml:4:",
        "infra/forge/carry.yaml:6:",
        "infra/forge/carry.yaml:8:",
        "infra/forge/carry.yaml:9:",
        "infra/forge/carry.yaml:12:",
        "infra/forge/carry.yaml:17:",
    ] {
        assert!(
            said.lines()
                .any(|l| l.contains(site) && l.contains("owner someone")),
            "{site} is refused:\n{said}"
        );
    }
    for clean in ["carry.yaml:3:", "carry.yaml:16:"] {
        assert!(
            !said.lines().any(|l| l.contains(clean)),
            "{clean} names no path outside the organisation:\n{said}"
        );
    }
}

/// BEHAVIOUR 3 — a file outside the scanned directories is not the lint's
/// business: a refusal test must name an owner that is refused, a recorded
/// packet keeps the head it recorded, and a person's record names their
/// GitHub handle as an IDENTITY (`github_username` on an employee row),
/// not a path. An UNTRACKED file under infra/ is not read either.
#[test]
fn files_outside_the_scanned_dirs_and_untracked_files_are_not_read() {
    let tree = Tree::new("elsewhere");
    tree.file(
        "crates/core/boss-testing/tests/some_refusal.rs",
        "const OTHER: &str = \"https://github.com/someone/boss-mirror.git\";\n",
    );
    tree.file(
        "examples/tenant/seeds/employees.json",
        "[{\"id\": \"emp-someone\", \"github_username\": \"someone\"}]\n",
    );
    tree.file("infra/estate/estate.toml", "[node]\nid = \"forge\"\n");
    let untracked = tree.0.join("infra/forge/notes.txt");
    scratch::create_dir(untracked.parent().unwrap());
    scratch::write_file(&untracked, "push to https://github.com/someone/boss.git\n");
    let out = tree.run();
    assert!(
        out.status.success(),
        "paths outside the scanned directories and an untracked file are not findings; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    // One fixture under infra/ and the history file, plus the lint and
    // every helper.
    let want = format!("scanned {} file(s)", 1 + 1 + 1 + lint_lib_count());
    assert!(text(&out).contains(&want), "{want}:\n{}", text(&out));
}

/// BEHAVIOUR 4 — the history allowance excuses its file and nothing else,
/// and goes stale loudly: an applied migration is never edited, so the
/// one that declared the retired token keeps its text — but when that
/// file is gone, an entry naming it is a hole shaped like a file, and the
/// lint refuses (lib/allowlist.sh).
#[test]
fn the_history_allowance_excuses_its_file_and_refuses_when_it_is_gone() {
    let tree = Tree::new("history");
    let out = tree.run();
    assert!(
        out.status.success(),
        "the history file's personal token path is excused; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    git(&tree.0, &["rm", "-q", "-f", HISTORY]);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "an allowance naming a file that is gone is refused:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("stale allowlist") && text(&out).contains(HISTORY),
        "{}",
        text(&out)
    );
}

/// BEHAVIOUR 5 — a tree git cannot list is exit 3, never clean.
#[test]
fn a_tree_it_cannot_read_is_a_refusal_not_a_pass() {
    let root = scratch::scratch_dir("no-github-path-outside-the-org-nogit");
    boss_testing::copy_lint_libs(&root);
    let body = std::fs::read_to_string(repo_root().join(LINT)).unwrap();
    scratch::write_exec(&root.join(LINT), &body);
    let out = Command::new("bash")
        .arg(root.join(LINT))
        .current_dir(&root)
        .env("GIT_CEILING_DIRECTORIES", root.parent().unwrap())
        .output()
        .expect("run the lint outside a repository");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(3),
        "no repository to list is exit 3 (the machine could not answer):\n{}",
        text(&out)
    );
}

/// The tree this lint ships in passes it: the off-site declaration's fork
/// target, its token slot, and the recovery kit's copy of that token are
/// gone, and nothing else under infra/, .forgejo/ or .github/ names a
/// GitHub path outside the organisation.
#[test]
fn the_shipped_tree_names_no_github_path_outside_the_org() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint on the tree");
    assert!(
        out.status.success(),
        "the shipped tree must be clean (exit {:?}):\n{}",
        out.status.code(),
        text(&out)
    );
}
