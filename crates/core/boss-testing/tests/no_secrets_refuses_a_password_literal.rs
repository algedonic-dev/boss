//! `infra/lint/no-secrets.sh` refuses a role statement that carries its
//! password as a quoted literal (backlog 7ec7113b).
//!
//! `111-gateway-audit-events.sql` created a LOGIN role with the password
//! equal to its name, and the lint passed it for six weeks: its only
//! password rule wants a 32-character opaque value after `password=` or
//! `password:`, and a SQL role statement spells neither. A migration runs
//! on every database the tree deploys to, so a literal there is a
//! credential every instance shares with every reader of the repo.
//!
//! Each case is planted in a throwaway git repo holding the lint as the
//! branch has it, and the lint is run on that tree, not on this one. The
//! shapes below are the one 111 actually has (a `CREATE ROLE … LOGIN
//! PASSWORD` inside a DO block) and its neighbours; the ones the lint must
//! let through are the spellings that carry no value — `PASSWORD NULL`,
//! a psql variable, a `format()` placeholder.
//!
//! The same review found two more spellings of a literal no rule could see
//! (backlog 9be722f0): a password inside a connection URL — the deleted
//! `infra/gateway/audit-events.conf` carried
//! `postgres://<role>:<role>@127.0.0.1/boss` for six weeks, because
//! `url-token` wanted http(s) and a 40-hex value — and a psql `\set` of a
//! password variable. `url-password` and `psql-set-password` are those two.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Tree {
    dir: PathBuf,
}

impl Tree {
    fn new(case: &str) -> Self {
        let dir = boss_testing::scratch_dir(&format!("no-secrets-role-{case}"));
        let root = repo_root();
        boss_testing::create_dir(&dir.join("infra/lint/lib"));
        boss_testing::create_dir(&dir.join("infra/postgres/schema"));
        for rel in [
            "infra/lint/no-secrets.sh",
            "infra/lint/no-secrets-allow.txt",
            "infra/lint/lib/git-answer.sh",
            "infra/lint/lib/scanned.sh",
        ] {
            std::fs::copy(root.join(rel), dir.join(rel))
                .unwrap_or_else(|e| panic!("copy {rel}: {e}"));
        }
        let me = Tree { dir };
        me.git(&["init", "-q", "-b", "main", "."]);
        me
    }

    fn git(&self, args: &[&str]) {
        let out = isolated(&mut Command::new("git"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Plant `body` at `rel`, track it, and run the lint over the tree.
    fn scan(&self, rel: &str, body: &str) -> (i32, String) {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            boss_testing::create_dir(parent);
        }
        boss_testing::write_file(&path, body);
        self.git(&["add", "-A"]);
        let out = isolated(&mut Command::new("bash"))
            .arg("infra/lint/no-secrets.sh")
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn no-secrets: {e}"));
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Git reads the fixture's config and nothing the host brings (the shape
/// `a_lint_that_cannot_read_does_not_say_clean.rs` uses).
fn isolated(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0")
        .env_remove("GIT_CONFIG_PARAMETERS")
}

/// A value planted in a fixture: assembled here so no role statement with
/// a literal exists in this file for the real tree's scan to find.
fn stmt(head: &str, value: &str) -> String {
    format!("{head} PASSWORD {q}{value}{q};\n", q = '\'')
}

fn assert_refused(case: &str, rel: &str, body: &str) {
    assert_refused_by("role-password", case, rel, body);
}

fn assert_refused_by(rule: &str, case: &str, rel: &str, body: &str) {
    let tree = Tree::new(case);
    let (code, out) = tree.scan(rel, body);
    assert_eq!(
        code, 1,
        "{case}: a password literal passed the lint (exit {code}):\n{out}"
    );
    assert!(
        out.contains(&format!("{rel}:")) && out.contains(&format!("[{rule}]")),
        "{case}: the finding must name the file and the `{rule}` rule:\n{out}"
    );
    // A SUFFIX of the value, not the whole: a redaction that stops after
    // the first character of the literal still leaks the rest of it.
    assert!(
        !out.contains("planted-value"),
        "{case}: the finding copied the password into the report — a lint that \
         screams about a secret must not print it:\n{out}"
    );
}

/// `PASSWORD <open><value><close>` with any quoting, assembled so the
/// statement never appears whole in this file.
fn quoted(open: &str, value: &str, close: &str) -> String {
    format!("PASSWORD {open}{value}{close}")
}

fn assert_passed(case: &str, rel: &str, body: &str) {
    let tree = Tree::new(case);
    let (code, out) = tree.scan(rel, body);
    assert_eq!(
        code, 0,
        "{case}: a role statement carrying no password value was refused:\n{out}"
    );
}

const MIGRATION: &str = "infra/postgres/schema/20990101000000-a-role.sql";

#[test]
fn the_shape_111_has_is_refused() {
    let body = format!(
        "DO $$\nBEGIN\n    {}EXCEPTION WHEN duplicate_object THEN\n    NULL;\nEND $$;\n",
        stmt("CREATE ROLE app_writer LOGIN", "s3cret-planted-value")
    );
    assert_refused("create-role-in-do", MIGRATION, &body);
}

#[test]
fn an_alter_and_a_user_and_lower_case_are_refused() {
    assert_refused(
        "alter-role",
        MIGRATION,
        &stmt("ALTER ROLE app_writer WITH", "s3cret-planted-value"),
    );
    assert_refused(
        "create-user-lower",
        "docs/setup.md",
        &stmt(
            "create user app_writer with encrypted",
            "s3cret-planted-value",
        ),
    );
}

/// The shapes the adversarial review of this car (2026-09-28) found the
/// first version of the rule blind to: the password on a later line than
/// the role keyword, dollar quoting, the escape and Unicode string forms,
/// and CREATE GROUP.
#[test]
fn every_quoting_and_a_later_line_are_refused() {
    let v = "s3cret-planted-value";
    for (case, body) in [
        (
            "multi-line",
            format!(
                "CREATE ROLE app_writer LOGIN\n    {};\n",
                quoted("'", v, "'")
            ),
        ),
        (
            "dollar",
            format!("ALTER ROLE app_writer {};\n", quoted("$$", v, "$$")),
        ),
        (
            "dollar-tag",
            format!("ALTER ROLE app_writer {};\n", quoted("$pw$", v, "$pw$")),
        ),
        (
            "escape",
            format!("ALTER ROLE app_writer {};\n", quoted("E'", v, "'")),
        ),
        (
            "unicode",
            format!("ALTER ROLE app_writer {};\n", quoted("U&'", v, "'")),
        ),
        (
            "group",
            format!("CREATE GROUP app_writers {};\n", quoted("'", v, "'")),
        ),
    ] {
        assert_refused(case, MIGRATION, &body);
    }
}

#[test]
fn a_statement_carrying_no_value_passes() {
    assert_passed(
        "quote-literal",
        MIGRATION,
        "EXECUTE 'ALTER ROLE ' || r || ' PASSWORD ' || quote_literal(pw);\n",
    );
    assert_passed(
        "password-null",
        MIGRATION,
        "ALTER ROLE app_writer NOLOGIN PASSWORD NULL;\n",
    );
    assert_passed(
        "psql-variable",
        MIGRATION,
        "CREATE ROLE app_writer LOGIN PASSWORD :'app_password';\n",
    );
    assert_passed(
        "format-placeholder",
        MIGRATION,
        "EXECUTE format('ALTER ROLE %I PASSWORD %L', r, pw);\n",
    );
}

/// 111 itself stays in the tree — it cannot be edited — so the real
/// tree's scan passes only because the allow-list names it, visibly;
/// the runbook's line is the scratch test server's role, whose URL
/// `TestDb` spells as its default. Each entry names ONE file: a glob
/// over the schema directory would let the next literal through.
#[test]
fn the_rule_allow_lists_two_named_files_and_no_glob() {
    let allow = std::fs::read_to_string(repo_root().join("infra/lint/no-secrets-allow.txt"))
        .unwrap_or_else(|e| panic!("reading the allow-list: {e}"));
    let entries: Vec<&str> = allow
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| l.ends_with(":role-password"))
        .collect();
    let expected = [
        "infra/postgres/schema/111-gateway-audit-events.sql:role-password",
        "docs/runbooks/dev-environment-bootstrap.md:role-password",
    ];
    assert_eq!(
        entries, expected,
        "the role-password allow entries changed — each must name one file, with a why"
    );
    assert!(
        Path::new(&repo_root().join("infra/postgres/schema/111-gateway-audit-events.sql")).exists(),
        "111 is gone — drop its allow entry with it"
    );
}

/// `<scheme>://<user>:<password>@<rest>`, assembled so no URL carrying a
/// literal password appears whole in this file.
fn url(scheme: &str, user: &str, password: &str, rest: &str) -> String {
    format!("{scheme}://{user}:{password}@{rest}")
}

/// The line the deleted `infra/gateway/audit-events.conf` carried, in its
/// own shape: a systemd drop-in, a Postgres URL, the role's name as its
/// password. Nothing refused it; this rule must.
#[test]
fn the_deleted_drop_ins_url_is_refused() {
    let role = "boss_gateway_audit";
    let body = format!(
        "[Service]\nEnvironment=BOSS_GATEWAY_AUDIT_DB_URL={}\n",
        url("postgres", role, role, "127.0.0.1/boss")
    );
    let tree = Tree::new("url-drop-in");
    let rel = "infra/gateway/audit-events.conf";
    let (code, out) = tree.scan(rel, &body);
    assert_eq!(code, 1, "the drop-in's URL passed the lint:\n{out}");
    assert!(
        out.contains(&format!("{rel}:2 [url-password]")),
        "the finding must name the file, the line and `url-password`:\n{out}"
    );
    assert!(
        !out.contains(role),
        "the finding copied the URL's credentials into the report:\n{out}"
    );
}

/// Any scheme, not only http(s) — and any password, not only 40 hex.
#[test]
fn a_literal_password_in_any_schemes_url_is_refused() {
    let v = "s3cret-planted-value";
    for scheme in ["postgresql", "mysql", "redis", "amqp", "https", "git+ssh"] {
        assert_refused_by(
            "url-password",
            &format!("url-{}", scheme.replace('+', "-")),
            "deploy/app.env",
            &format!(
                "DATABASE_URL={}\n",
                url(scheme, "app", v, "db.internal:5432/app")
            ),
        );
    }
}

/// A URL whose password is interpolated, formatted in, or already redacted
/// carries no value: the credential lives wherever the variable is set.
/// The last case is a host and port followed by a path holding an email
/// address — no userinfo at all, since a URL's userinfo cannot hold a `/`.
#[test]
fn a_url_carrying_no_password_value_passes() {
    for (case, body) in [
        (
            "shell-var-braced",
            format!(
                "DB={}\n",
                url("postgres", "boss", "${PGPASSWORD}", "db/boss")
            ),
        ),
        (
            "shell-var-default",
            format!(
                "DB={}\n",
                url("postgres", "${PGUSER}", "${PGPASSWORD:-}", "db/boss")
            ),
        ),
        (
            "shell-var-bare",
            format!("DB={}\n", url("postgres", "$user", "$pw", "db/boss")),
        ),
        (
            "rust-format",
            format!(
                "let u = format!(\"{}\");\n",
                url("postgres", "boss", "{password}", "db/boss")
            ),
        ),
        (
            "printf",
            format!(
                "printf '{}' \"$t\"\n",
                url("https", "oauth2", "%s", "forge/boss.git")
            ),
        ),
        (
            "redacted",
            format!("# prints {}\n", url("postgres", "boss", "***", "db/boss")),
        ),
        (
            "host-port-path",
            "GET http://people:7910/api/people/by-email/someone@example.org/x\n".to_string(),
        ),
    ] {
        let tree = Tree::new(&format!("url-pass-{case}"));
        let (code, out) = tree.scan("deploy/app.env", &body);
        assert_eq!(
            code, 0,
            "{case}: a URL carrying no password value was refused:\n{out}"
        );
    }
}

/// The quickstart's demo `boss:boss` passes because the allow-list names
/// its file — and ONLY because: the same line anywhere else is refused.
#[test]
fn the_quickstart_demo_url_passes_only_through_the_allow_list() {
    let line = format!(
        "      DATABASE_URL: {}\n",
        url("postgres", "boss", "boss", "postgres:5432/boss")
    );
    let tree = Tree::new("url-compose-allowed");
    let (code, out) = tree.scan("infra/oss-quickstart/docker-compose.yml", &line);
    assert_eq!(code, 0, "the allow-listed compose file was refused:\n{out}");
    assert!(
        out.contains("allowed infra/oss-quickstart/docker-compose.yml:1 [url-password]"),
        "the suppression must be printed, not silent:\n{out}"
    );
    assert_refused_by(
        "url-password",
        "url-compose-elsewhere",
        "infra/elsewhere/docker-compose.yml",
        &line,
    );
}

/// `\set <name holding pass or pw> <value>` in a psql script is a literal
/// password. A psql variable reference or a backtick command carries none.
#[test]
fn a_psql_set_of_a_password_literal_is_refused() {
    // Assembled around `{bs}` so no `\set` of a literal exists in this file.
    let set = |name: &str, value: &str| format!("{bs}set {name} {value}\n", bs = '\\');
    let v = "s3cret-planted-value";
    for (case, body) in [
        ("quoted", set("gateway_password", &format!("'{v}'"))),
        ("upper", set("APP_PASS", &format!("'{v}'"))),
        ("pw", set("pw", &format!("'{v}'"))),
        ("bare", set("app_passwd", v)),
    ] {
        assert_refused_by(
            "psql-set-password",
            &format!("psql-{case}"),
            "infra/postgres/seed.psql",
            &body,
        );
    }
    for (case, body) in [
        ("from-var", "\\set app_password :'env_password'\n"),
        ("from-shell", "\\set app_password `cat /run/secrets/pw`\n"),
        ("not-a-password", "\\set ON_ERROR_STOP on\n"),
    ] {
        let tree = Tree::new(&format!("psql-pass-{case}"));
        let (code, out) = tree.scan("infra/postgres/seed.psql", body);
        assert_eq!(
            code, 0,
            "{case}: a \\set carrying no password literal was refused:\n{out}"
        );
    }
}

/// Every `url-password` allow entry names ONE file that exists: a glob
/// would pass the next literal written beside the demo one, and an entry
/// whose file is gone is a silencer waiting for a new file at that path.
#[test]
fn the_url_password_allow_entries_name_one_existing_file_each() {
    let allow = std::fs::read_to_string(repo_root().join("infra/lint/no-secrets-allow.txt"))
        .unwrap_or_else(|e| panic!("reading the allow-list: {e}"));
    let paths: Vec<&str> = allow
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter_map(|l| l.strip_suffix(":url-password"))
        .collect();
    assert!(
        paths.contains(&"infra/oss-quickstart/docker-compose.yml"),
        "the quickstart's demo URL must pass through a named allow entry"
    );
    for path in paths {
        assert!(
            !path.contains(['*', '?', '[']),
            "url-password allow entry {path:?} is a glob — name one file"
        );
        assert!(
            repo_root().join(path).is_file(),
            "url-password allow entry {path:?} names no file — drop it"
        );
    }
}
