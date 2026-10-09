//! `machine_gate_value_header` and `machine_token_header`, the two
//! functions in `infra/lib/secret-header.sh` that write the machine-token
//! header, decide WHICH HOSTS may be sent one by the same rule on the
//! same list (backlog d26515c5, unit 7; adversarial review 4d39f4dc, F2).
//!
//! THE DEFECT THIS HOLDS. The value door arrived with its own copy of
//! the list's resolution, and the reviewer's table (equiv.sh, 286 rows)
//! found the copies already apart on four: loopback hosts when the
//! rendered sor.env is present and cannot be read — the mount's reader
//! stamped, the value door withheld. Nothing held them equal (CLAUDE.md
//! §9a). They are now ONE resolution both call
//! (`_secret_header_mt_hosts`), and this is the reviewer's table, run
//! under bash and dash, with no row allowed to differ. An unreadable
//! list is no list for both: loopback is the rule's own and needs none.
//!
//! Fixture values are fake and nothing is sent: the functions only write
//! a header file or decline to.
//!
//! A THIRD READER OF THE SAME RULE (backlog 7369b078, F1 of review
//! 9c484ca8). `machine_token_admits URL` is the judgement alone, with no
//! header written, for a caller whose secret is NOT the machine token:
//! the ops runner asks it before it makes its own credential's header.
//! It calls the same two functions, and the table's fifth column holds
//! it to the other two on every row, so a credential of another name
//! cannot come to be judged by a second copy of the rule.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::process::Command;

const LIB: &str = "infra/lib/secret-header.sh";
const MOUNTED: &str = "fakeMOUNTfakeMOUNTfakeMOUNTfakeMOUNTfake904";
const HELD: &str = "fakeVALUEfakeVALUEfakeVALUEfakeVALUEfake905";

/// The reviewer's rows: thirteen ways the list can be given, by twenty-two
/// addresses, and the six scheme-less shapes of review 927f8602 (F1).
/// POSIX sh, because the lib is.
const TABLE: &str = r#"
set -u
export TMPDIR="$D/tmp" BOSS_MACHINE_TOKEN_DIR="$D/mount"
unset RUNTIME_DIRECTORY
trap 'true' EXIT
. "$LIB"
URLS="http://10.20.0.34:7900 http://10.20.0.34 https://10.20.0.34:443 http://10.20.0.35:7900 http://127.0.0.1:7900 http://localhost:1 http://LOCALHOST:1 http://127.1:1 http://[::1]:1 http://jobs.boss.svc.cluster.local:7900 http://boss.svc.cluster.local:1 http://evilboss.svc.cluster.local:1 http://x.boss.svc.cluster.local.evil.example:1 http://10.20.0.34@evil.example:1 http://evil.example#@10.20.0.34 http://evil.example/10.20.0.34 http://10.20.0.34.:7900 http://10.20.0.340:1 http://evil.example:1 10.20.0.34:7900 //10.20.0.34 http:// evil.example/x://10.20.0.34 evil.example/x://127.0.0.1 evil.example:80/x://10.20.0.34:7900 evil.example?x=://10.20.0.34 evil.example#://10.20.0.34 user@evil.example/://localhost"
row() {
    label="$1"
    for url in $URLS; do
        A="" B=""
        machine_token_header A "$url" 2>/dev/null
        machine_gate_value_header B "$url" "$VALUE" 2>/dev/null
        a=withheld; b=withheld
        if [ -n "$A" ]; then a=stamped; fi
        if [ -n "$B" ]; then b=stamped; fi
        c=withheld
        if machine_token_admits "$url"; then c=stamped; fi
        printf '%s\t%s\t%s\t%s\t%s\n' "$label" "$url" "$a" "$b" "$c"
    done
}
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/sor.env";        row "list from sor.env"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/absent.env";     row "no env, no sor.env"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/unreadable.env"; row "sor.env is a directory"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/two.env";        row "two lines, first wins"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/crlf.env";       row "CRLF line"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/emptyline.env";  row "empty line in sor.env"
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/noline.env";     row "sor.env without the line"
export BOSS_MACHINE_TOKEN_HOSTS="";                        export BOSS_SOR_ENV="$D/sor.env"; row "env set but empty beats sor.env"
export BOSS_MACHINE_TOKEN_HOSTS="evil.example";            row "env names another host"
export BOSS_MACHINE_TOKEN_HOSTS="10.20.0.34 evil.example"; row "env, whitespace list"
export BOSS_MACHINE_TOKEN_HOSTS=".example,10.20.0.34";     row "env, suffix entry"
export BOSS_MACHINE_TOKEN_HOSTS="*";                       row "env, a star"
export BOSS_MACHINE_TOKEN_HOSTS=".";                       row "env, a bare dot"
# What each wrote, for one host both serve: the mount's token and the
# caller's value, never the other way round.
unset BOSS_MACHINE_TOKEN_HOSTS; export BOSS_SOR_ENV="$D/sor.env"
machine_token_header A "http://10.20.0.34:7900"
machine_gate_value_header B "http://10.20.0.34:7900" "$VALUE"
printf 'wrote\tmount\t%s\t%s\n' "$(cat "${A#@}")" "$(stat -c %a "${A#@}")"
printf 'wrote\tvalue\t%s\t%s\n' "$(cat "${B#@}")" "$(stat -c %a "${B#@}")"
machine_gate_value_header E "http://10.20.0.34:7900" ""
printf 'edge\tempty value\t%s\t[%s]\n' "$?" "$E"
machine_gate_value_header N "http://10.20.0.34:7900" "fake
x-boss-user: admin" 2>/dev/null
printf 'edge\tline break\t%s\t[%s]\n' "$?" "$N"
machine_gate_value_header 9bad "http://10.20.0.34:7900" "$VALUE" 2>/dev/null
printf 'edge\tbad name\t%s\t[]\n' "$?"
"#;

fn rows(shell: &str) -> Vec<Vec<String>> {
    let d = scratch_dir(&format!("value-header-equality-{shell}"));
    for sub in ["mount", "tmp", "unreadable.env"] {
        create_dir(&d.join(sub));
    }
    write_file(&d.join("mount/current"), &format!("{MOUNTED}\n"));
    write_file(
        &d.join("sor.env"),
        "BOSS_JOBS_URL=http://10.20.0.34:7900\nBOSS_MACHINE_TOKEN_HOSTS=10.20.0.34,.boss.svc.cluster.local\n",
    );
    write_file(
        &d.join("two.env"),
        "BOSS_MACHINE_TOKEN_HOSTS=10.20.0.34\nBOSS_MACHINE_TOKEN_HOSTS=evil.example\n",
    );
    write_file(
        &d.join("crlf.env"),
        "BOSS_MACHINE_TOKEN_HOSTS=10.20.0.34\r\n",
    );
    write_file(&d.join("emptyline.env"), "BOSS_MACHINE_TOKEN_HOSTS=\n");
    write_file(&d.join("noline.env"), "# nothing\n");
    let out = Command::new(shell)
        .args(["-c", TABLE])
        .env("D", &d)
        .env("LIB", repo_root().join(LIB))
        .env("VALUE", HELD)
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .env_remove("BOSS_SOR_ENV")
        .output()
        .unwrap_or_else(|e| panic!("{shell}: {e}"));
    assert!(
        out.status.success(),
        "[{shell}] the table ran: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

/// THE TWO DECIDE THE SAME HOSTS, ON EVERY ROW. No intended difference:
/// if one is ever wanted it is named here, with its reason.
#[test]
fn both_header_writers_decide_the_same_hosts() {
    for shell in ["bash", "dash"] {
        let all = rows(shell);
        let table: Vec<&Vec<String>> = all
            .iter()
            .filter(|r| r[0] != "wrote" && r[0] != "edge")
            .collect();
        assert_eq!(table.len(), 13 * 28, "[{shell}] every row ran");
        let differing: Vec<String> = table
            .iter()
            .filter(|r| r[2] != r[3] || r[3] != r[4])
            .map(|r| {
                format!(
                    "[{}] {}: machine_token_header={} machine_gate_value_header={} \
                     machine_token_admits={}",
                    r[0], r[1], r[2], r[3], r[4]
                )
            })
            .collect();
        assert!(
            differing.is_empty(),
            "[{shell}] the two header writers decide these hosts differently — the host rule's \
             list must have ONE resolution (CLAUDE.md §9a):\n  {}",
            differing.join("\n  ")
        );
        // The table is not vacuous: each answer occurs, and the rows the
        // reviewer measured apart now read the same, the safe way round.
        let said = |label: &str, url: &str| -> String {
            table
                .iter()
                .find(|r| r[0] == label && r[1] == url)
                .map(|r| r[3].clone())
                .unwrap_or_else(|| panic!("[{shell}] no row {label} / {url}"))
        };
        assert_eq!(
            said("list from sor.env", "http://10.20.0.34:7900"),
            "stamped"
        );
        assert_eq!(
            said("list from sor.env", "http://evil.example:1"),
            "withheld"
        );
        assert_eq!(
            said("list from sor.env", "http://10.20.0.34@evil.example:1"),
            "withheld"
        );
        assert_eq!(
            said("env set but empty beats sor.env", "http://10.20.0.34:7900"),
            "withheld"
        );
        assert_eq!(
            said("sor.env is a directory", "http://127.0.0.1:7900"),
            "stamped",
            "[{shell}] an unreadable list is no list, and loopback needs none"
        );
        assert_eq!(
            said("sor.env is a directory", "http://10.20.0.34:7900"),
            "withheld",
            "[{shell}] an unreadable list names no host"
        );
        // NO LEADING SCHEME, NO HOST (backlog 50708d76, F1 of review
        // 927f8602). The six rows the reviewer measured against curl —
        // each read ADMIT by the host after a later `://` while curl
        // connected to the first — and the bare `host:port` beside them
        // are withheld by all three readers under EVERY list, the one
        // that names the first host included: a URL the rule cannot read
        // a host from is not judged by a guess at curl's.
        for url in [
            "evil.example/x://10.20.0.34",
            "evil.example/x://127.0.0.1",
            "evil.example:80/x://10.20.0.34:7900",
            "evil.example?x=://10.20.0.34",
            "evil.example#://10.20.0.34",
            "user@evil.example/://localhost",
            "10.20.0.34:7900",
        ] {
            let stamped: Vec<&str> = table
                .iter()
                .filter(|r| r[1] == url && r[2..5].iter().any(|c| c != "withheld"))
                .map(|r| r[0].as_str())
                .collect();
            assert_eq!(
                table.iter().filter(|r| r[1] == url).count(),
                13,
                "[{shell}] {url} ran under every list"
            );
            assert!(
                stamped.is_empty(),
                "[{shell}] {url} has no leading http:// or https:// and was stamped under: \
                 {stamped:?}"
            );
        }
    }
}

/// WHAT EACH WRITES: the mount's reader writes the mounted token, the
/// value door the value it was handed — one header line, 0600 — and the
/// door declines an empty value, a value with a line break and a name
/// that is no variable.
#[test]
fn the_value_door_writes_the_value_it_was_handed_and_nothing_else() {
    for shell in ["bash", "dash"] {
        let all = rows(shell);
        let find = |kind: &str, what: &str| -> Vec<String> {
            all.iter()
                .find(|r| r[0] == kind && r[1] == what)
                .cloned()
                .unwrap_or_else(|| panic!("[{shell}] no {kind} row for {what}"))
        };
        let mount = find("wrote", "mount");
        assert!(
            mount[2] == format!("x-boss-machine-token: {MOUNTED}") && mount[3] == "600",
            "[{shell}] the mount's reader wrote something else, or not 0600"
        );
        let value = find("wrote", "value");
        assert!(
            value[2] == format!("x-boss-machine-token: {HELD}") && value[3] == "600",
            "[{shell}] the value door wrote something else, or not 0600"
        );
        assert_eq!(find("edge", "empty value")[2..], ["0", "[]"], "[{shell}]");
        assert_eq!(find("edge", "line break")[2..], ["2", "[]"], "[{shell}]");
        assert_eq!(find("edge", "bad name")[2..], ["2", "[]"], "[{shell}]");
    }
}
