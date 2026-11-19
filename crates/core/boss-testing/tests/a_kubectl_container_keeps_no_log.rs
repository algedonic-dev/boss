//! A container handed a credential, or running a cluster client, keeps
//! no container log.
//!
//! The forge has no kubectl on the host, so every forge script reaches
//! the cluster as `docker run … alpine/k8s … kubectl`, and kubectl's
//! answer comes back on the CONTAINER's stdout. When the answer is a
//! Secret — the deposit private key (runner-credential-push.sh, and
//! runner-credential-send.sh through the line it is handed), the ops
//! runner credential (runner-credential-deposit.sh), the forge token,
//! the machine token, the registry and probe-reader credentials, the
//! offsite service-account key read through ops_kubectl, the Secrets
//! cluster-deploy-runner.sh copies between namespaces — the docker
//! daemon's log driver writes those bytes wherever it writes, BESIDE the
//! pipe the script reads them from.
//!
//! MEASURED 2026-10-10 (backlog aa700921, from re-review 34f8448c of car
//! 2c628718), through read-only ops doors only: `forge-log` reports the
//! system daemon's `forgejo` container on `json-file {}` (no rotation
//! options), and `unit-cat docker.service` shows dockerd started with no
//! `--log-driver` flag; no door reads /etc/docker/daemon.json. So each of
//! these runs wrote its Secret to /var/lib/docker/containers/<id>/
//! <id>-json.log for the container's life; `--rm` unlinks the file,
//! which frees the blocks and does not erase them, and a run the daemon
//! never reaps (a daemon restart mid-run) keeps the file. A daemon
//! default ever moved to journald or syslog would keep every value in a
//! journal that outlives the container entirely.
//!
//! THE RULE: every `docker run` in infra/ that runs kubectl (or the
//! talosctl `ops_talosctl` mounts into the same image) OR is handed
//! anything from the host through `-v` / `--mount` carries
//! `--log-driver=none` AMONG ITS OWN OPTIONS, before the image. The
//! docker CLI attaches to the container's stdout before it starts it,
//! and the attach stream is not the log driver, so the script still
//! reads every byte; nothing else does. The rule covers EVERY kubectl
//! container, not only the lines that read a Secret today, because the
//! kubectl line is shared: one `ops_kubectl` serves every verb, and any
//! later `get secret` through any of these lines would otherwise inherit
//! the log silently.
//!
//! WIDENED 2026-10-18 (backlog ff957587, from review faa40da8 of car
//! 2afb79b4), on three findings the first version let through:
//!   - THE RECOVERY SHEET. infra/recovery/re-entry.toml prints four
//!     forge-side `docker run … kubectl` lines an operator types by hand,
//!     one of them `-it … exec -it` into the dev workspace, so a whole
//!     recovery session's terminal went to the container's json log. The
//!     pin scanned shell only; it now reads every `command = "…"` value
//!     in infra/'s TOML, and the `docker run` argv lists infra/'s Python
//!     builds.
//!   - THE PLACE OF THE FLAG. The pin accepted `--log-driver=none`
//!     anywhere on the line; after the image it is kubectl's argument,
//!     not docker's, and the container logs. It now walks docker's
//!     options and stops at the image. It also joined lines only on a
//!     trailing backslash, so an array `KUBECTL=(docker run …` written
//!     across lines without one was read as a run of nothing; a command
//!     now continues while a quote or a parenthesis is open, bounded so
//!     one apostrophe in prose cannot swallow a file.
//!   - THE SCOPE. Three containers that mount a credential and run no
//!     cluster client were outside a rule keyed on the program: the
//!     ops-credentials.sh stat door (all of /etc/boss-ops), the
//!     check-gcs-backup.sh readback (the offsite service-account key) and
//!     the talosctl identity run admission-reader-identity.py builds. The
//!     rule is keyed on the MOUNT, every mount, not on a list of
//!     credential spellings: a list is a guess about which file is
//!     secret, and the stat door's mount source is `$_oc_dir`, which no
//!     spelling list would have named. A mount of a plain script costs a
//!     log nobody reads; a mount of a key missed costs the key.
//!
//! tree-wide pin — it scans infra/, which no changed-file map
//! attributes to this crate, so every scoped gate runs it whatever its
//! scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// Built in halves so this file's own text is not an instance.
const NO_LOG: &str = concat!("--log-driver", "=none");
const DOCKER_RUN: &str = concat!("docker ", "run");

/// At most this many physical lines are read as one command while a
/// quote or parenthesis stays open: the longest real one in the tree is
/// a dozen, and an unbalanced apostrophe in prose must not join a file.
const MAX_JOIN: usize = 40;

/// docker run's options that take a value as the NEXT word (unless
/// spelled `--flag=value`). An option in neither this list nor
/// `BOOLEAN`, spelled without `=`, REFUSES its run by name: the reader
/// cannot tell its value from the image, and reading on would judge the
/// mounts after it by a guess. Until 2026-10-20 it read the value as the
/// image and stopped, so a `-v` after `--shm-size 64m` was never seen and
/// the run passed as mounting nothing — a false pass this comment claimed
/// could not happen (review f5ae501b of car 1ba5e676, backlog b21faf52).
const VALUED: &[&str] = &[
    "-v",
    "--volume",
    "--mount",
    "--network",
    "--net",
    "--name",
    "-m",
    "--memory",
    "--memory-swap",
    "--pids-limit",
    "--cap-add",
    "--cap-drop",
    "--security-opt",
    "--tmpfs",
    "-e",
    "--env",
    "--env-file",
    "--ulimit",
    "--entrypoint",
    "-w",
    "--workdir",
    "-u",
    "--user",
    "--pull",
    "--log-driver",
    "--log-opt",
    "-l",
    "--label",
    "-h",
    "--hostname",
    "--add-host",
    "--device",
    "--platform",
    "--cpus",
    "--gpus",
    "-p",
    "--publish",
];

/// docker run's options that take no value, as the tree spells them
/// (short flags bundled the way they are written). See `VALUED`. These
/// four are every one infra/ spelled on 2026-10-20: the pin, run with
/// only `--rm` here, named `--read-only` (check-gcs-backup.sh) and `-i`
/// (reap-terminated-pods.sh, retire-volume-claim.sh); `-it` is the
/// recovery sheet's.
const BOOLEAN: &[&str] = &["--rm", "-i", "-it", "--read-only"];

/// The options that hand the container something from the host.
const MOUNTS: &[&str] = &["-v", "--volume", "--mount"];

/// What a file is read as.
#[derive(Clone, Copy)]
enum Shape {
    Shell,
    /// Only `command = "…"` values: a recovery sheet's typed lines.
    TomlCommands,
    /// Only `["…", "docker", "run", …]` argv lists.
    Python,
}

/// Every file under `dir` the rule reads, with its shape: `*.sh` or a
/// shebang file (the pod's doors under infra/dev carry no extension),
/// `*.toml`, `*.py`.
fn files(dir: &Path, out: &mut Vec<(PathBuf, Shape)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|x| x == "toml") {
            out.push((p, Shape::TomlCommands));
        } else if p.extension().is_some_and(|x| x == "py") {
            out.push((p, Shape::Python));
        } else if p.extension().is_some_and(|x| x == "sh")
            || std::fs::read_to_string(&p).is_ok_and(|t| t.starts_with("#!"))
        {
            out.push((p, Shape::Shell));
        }
    }
}

/// The open quote / parenthesis state carried from one physical line to
/// the next.
#[derive(Default)]
struct Open {
    single: bool,
    double: bool,
    depth: usize,
}

impl Open {
    /// Read one physical line; true when it ends on a backslash that
    /// continues it.
    fn read(&mut self, line: &str) -> bool {
        let mut escaped = false;
        let mut after_space = true;
        for c in line.chars() {
            if self.single {
                self.single = c != '\'';
                after_space = false;
                continue;
            }
            if escaped {
                escaped = false;
                after_space = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '\'' if !self.double => self.single = true,
                '"' => self.double = !self.double,
                // A comment runs to the end of the line, so an apostrophe
                // in it opens nothing.
                '#' if !self.double && after_space => return false,
                '(' => self.depth += 1,
                ')' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            after_space = c.is_whitespace();
        }
        escaped
    }

    fn is_open(&self) -> bool {
        self.single || self.double || self.depth > 0
    }
}

/// A logical command: its text, and for each physical line in it the
/// byte offset it starts at and its line number.
struct Command {
    text: String,
    starts: Vec<(usize, usize)>,
}

impl Command {
    /// The line number of the byte at `at`.
    fn line_of(&self, at: usize) -> usize {
        self.starts
            .iter()
            .take_while(|(off, _)| *off <= at)
            .last()
            .map_or(0, |(_, n)| *n)
    }
}

/// The logical commands of a shell text: physical lines joined across a
/// trailing backslash or an open quote or parenthesis (at most
/// `MAX_JOIN`). A line that starts as a comment is prose about the rule,
/// never an instance of it.
fn commands(text: &str) -> Vec<Command> {
    let mut out = Vec::new();
    let mut cur: Option<(Command, Open)> = None;
    for (i, line) in text.lines().enumerate() {
        let (mut cmd, mut open) = match cur.take() {
            Some(c) => c,
            None => {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                (
                    Command {
                        text: String::new(),
                        starts: Vec::new(),
                    },
                    Open::default(),
                )
            }
        };
        cmd.starts.push((cmd.text.len(), i + 1));
        let continued = open.read(line);
        cmd.text
            .push_str(line.trim_end().strip_suffix('\\').unwrap_or(line));
        cmd.text.push(' ');
        if (continued || open.is_open()) && cmd.starts.len() < MAX_JOIN {
            cur = Some((cmd, open));
        } else {
            out.push(cmd);
        }
    }
    out.extend(cur.map(|(c, _)| c));
    out
}

/// One container run the rule judges.
#[derive(Debug, PartialEq)]
struct Run {
    line: usize,
    /// It runs a cluster client or is handed something from the host.
    in_scope: bool,
    /// `--log-driver=none` is among docker's own options.
    quiet: bool,
    /// The first option in neither `VALUED` nor `BOOLEAN`, spelled
    /// without `=`: the reader cannot tell its value from the image.
    unplaced: Option<String>,
}

/// A word stripped of the quoting around it in any of the three shapes.
fn bare(w: &str) -> &str {
    w.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | '\\' | ','))
}

/// Judge one run from the words after `docker run` (`words`) and the
/// whole text it is followed by (`rest`), which names its program.
fn judge<'a>(line: usize, words: impl IntoIterator<Item = &'a str>, rest: &str) -> Run {
    let mut words = words.into_iter().map(bare).filter(|w| !w.is_empty());
    let (mut quiet, mut mounts, mut unplaced) = (false, false, None);
    while let Some(w) = words.next() {
        if w.contains(NO_LOG) {
            // `%s--log-driver=none`: a printf placeholder for an optional
            // `-i ` joined to it (ops_kubectl) is still docker's option.
            quiet = true;
            continue;
        }
        if !w.starts_with('-') {
            break; // the image: every word after it is the program's
        }
        let name = w.split('=').next().unwrap_or(w);
        mounts |= MOUNTS.contains(&name);
        if w.contains('=') || BOOLEAN.contains(&name) {
            continue;
        }
        if !VALUED.contains(&name) {
            unplaced = Some(name.to_string());
            break;
        }
        words.next();
    }
    let client = rest
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .any(|w| w == "kubectl" || w == "talosctl");
    Run {
        line,
        in_scope: mounts || client,
        quiet,
        unplaced,
    }
}

/// Every `docker run` in a shell command, each judged on its own words
/// up to the next one.
fn shell_runs(cmd: &Command, out: &mut Vec<Run>) {
    let at: Vec<usize> = cmd.text.match_indices(DOCKER_RUN).map(|(i, _)| i).collect();
    for (k, &start) in at.iter().enumerate() {
        let end = at.get(k + 1).copied().unwrap_or(cmd.text.len());
        let seg = &cmd.text[start + DOCKER_RUN.len()..end];
        out.push(judge(cmd.line_of(start), seg.split_whitespace(), seg));
    }
}

/// Every run in `text`, read in its shape.
fn runs(text: &str, shape: Shape) -> Vec<Run> {
    let mut out = Vec::new();
    match shape {
        Shape::Shell => commands(text).iter().for_each(|c| shell_runs(c, &mut out)),
        Shape::TomlCommands => {
            for (i, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("command = ") {
                    let cmd = Command {
                        text: line.to_string(),
                        starts: vec![(0, i + 1)],
                    };
                    shell_runs(&cmd, &mut out);
                }
            }
        }
        Shape::Python => {
            for (i, line) in text.lines().enumerate() {
                for q in ['"', '\''] {
                    let open = format!("{q}docker{q}, {q}run{q}");
                    let Some(at) = line.find(&open) else {
                        continue;
                    };
                    // The list's literals up to its `]`, across lines.
                    let from = text.lines().take(i).map(|l| l.len() + 1).sum::<usize>() + at;
                    let list = &text[from..];
                    let list = &list[..list.find(']').unwrap_or(list.len())];
                    let literals: Vec<&str> = list.split(q).skip(1).step_by(2).collect();
                    out.push(judge(i + 1, literals.into_iter().skip(2), list));
                }
            }
        }
    }
    out
}

/// The runs the rule covers, as (line, keeps no log).
fn covered(text: &str, shape: Shape) -> Vec<(usize, bool)> {
    runs(text, shape)
        .into_iter()
        .filter(|r| r.in_scope)
        .map(|r| (r.line, r.quiet))
        .collect()
}

/// The runs that are not quiet and carry an option the reader cannot
/// place, as (line, option): whether they mount anything is unknown.
fn unplaced(text: &str, shape: Shape) -> Vec<(usize, String)> {
    runs(text, shape)
        .into_iter()
        .filter(|r| !r.quiet)
        .filter_map(|r| r.unplaced.map(|o| (r.line, o)))
        .collect()
}

#[test]
fn the_reader_sees_the_shapes_the_tree_spells() {
    // Shapes copied from the tree as it stood on 2026-10-10: an array, a
    // quoted line, a printf format, a backslash-continued run, and the
    // prose and non-kubectl runs that are NOT instances.
    let text = concat!(
        "KUBECTL=(docker run --rm --network host -v \"$KC:/kc:ro\" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc)\n",
        "    KUBECTL_LINE=\"docker run --rm --network host -v $KC:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc\"\n",
        "    printf 'sudo -n docker run --rm --log-driver=none --network host %s kubectl --kubeconfig=/kc\\n' \"$img\"\n",
        "report=\"$(sudo -n docker run --rm -i \\\n",
        "    alpine/k8s:1.33.3 kubectl get pods)\"\n",
        "# the runner's KAPPLY; a docker run of kubectl that reads stdin\n",
        "docker run --rm --entrypoint /usr/local/bin/boss-launch \"$image\" --check\n",
        "KDEL=(docker run -i \"${KUBECTL[@]:2}\")\n",
        "    printf 'sudo -n docker run --rm --network host %s timeout %s /talosctl --talosconfig=/tc\\n' \\\n",
        "        \"$img\" \"$t\"\n",
    );
    assert_eq!(
        covered(text, Shape::Shell),
        vec![(1, false), (2, false), (3, true), (4, false), (9, false)],
        "the reader must find the array, the quoted line, the printf, the continued run and the \
         talosctl — and neither the comment nor a run of anything but a cluster client"
    );
}

#[test]
fn the_flag_counts_only_among_dockers_own_options() {
    // Review faa40da8 finding 2: after the image the flag is the
    // program's argument, and the container logs.
    let text = concat!(
        "docker run --rm --network host -v \"$KC:/kc:ro\" alpine/k8s:1.33.3 kubectl --log-driver=none get pods\n",
        "docker run --rm --name \"$C\" --log-driver=none --network host -v \"$KC:/kc:ro\" alpine/k8s:1.33.3 kubectl get pods\n",
        "printf 'sudo -n docker run --rm %s--log-driver=none --network host --mount type=bind,src=%s,dst=/kc,readonly %s kubectl\\n' \"$i\" \"$k\" \"$img\"\n",
    );
    assert_eq!(
        covered(text, Shape::Shell),
        vec![(1, false), (2, true), (3, true)]
    );
}

#[test]
fn a_command_continues_while_a_quote_or_parenthesis_is_open() {
    // Review faa40da8 finding 2: an array written across lines with no
    // backslash was read as a run of nothing. A comment's apostrophe
    // opens nothing, and the run is reported on its own line.
    let text = concat!(
        "x=1  # it's prose\n",
        "KUBECTL=(sudo -n docker run --rm --network host\n",
        "    -v \"$KC:/kc:ro\" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc)\n",
        "echo done\n",
    );
    assert_eq!(covered(text, Shape::Shell), vec![(2, false)]);
}

#[test]
fn a_container_handed_a_mount_is_covered_whatever_it_runs() {
    // Review faa40da8 finding 3, in the shapes the three sites have.
    let text = concat!(
        "_oc_door=\"$(timeout -k 5 30 sudo -n docker run --rm --network none \\\n",
        "    --mount \"type=bind,src=$_oc_dir,dst=/ops,readonly\" alpine/k8s:1.33.3 \\\n",
        "    timeout 10 stat -c '%n' /ops/. 2>&1)\" || :\n",
        "timeout 900 sudo -n docker run --rm --name \"$CONTAINER\" --log-driver=none --read-only \\\n",
        "    --mount \"type=bind,src=$WORK/sa.json,dst=/gcs/sa.json,readonly\" \\\n",
        "    \"$IMAGE\" timeout 840 python3 /check.py\n",
        "timeout 120 sudo -n docker run --rm --name \"$C\" --network none --read-only \\\n",
        "    --tmpfs /tmp:rw,size=64m \"$IMAGE\" python3 -c 'import ssl'\n",
    );
    assert_eq!(covered(text, Shape::Shell), vec![(1, false), (4, true)]);
    let py = concat!(
        "    argv = [\"sudo\", \"-n\", \"docker\", \"run\", \"--pull=never\", \"--rm\",\n",
        "            \"--mount\", f\"type=bind,src={binary},dst=/talosctl,readonly\"]\n",
        "TALOS = ['sudo', '-n', 'docker', 'run', '--rm', '--log-driver=none', '--network', 'host',\n",
        "         '-v', '/kc:/kc:ro', 'img', 'talosctl']\n",
    );
    assert_eq!(covered(py, Shape::Python), vec![(1, false), (3, true)]);
}

#[test]
fn an_option_the_reader_cannot_place_refuses_the_run() {
    // Review f5ae501b of car 1ba5e676 (backlog b21faf52): the two
    // fixtures it measured GREEN on the tree-wide pin. An option in
    // neither list made its value read as the image, so the mount after
    // it was never seen and the run passed as mounting nothing.
    let text = concat!(
        "sudo -n docker run --rm --shm-size 64m -v /etc/boss-ops:/ops:ro alpine/k8s:1.33.3 cat /ops/kubeconfig\n",
        "sudo -n docker run --rm --stop-timeout 5 --mount type=bind,src=/etc/boss-ops,dst=/ops,readonly alpine/k8s:1.33.3 cat /ops/kubeconfig\n",
        // Controls: a value joined by `=` is placed whatever the option,
        // and a run already quiet before the option needs no reading.
        "sudo -n docker run --rm --shm-size=64m -v /etc/boss-ops:/ops:ro alpine/k8s:1.33.3 cat /ops/kubeconfig\n",
        "sudo -n docker run --rm --log-driver=none --stop-timeout 5 -v /etc/boss-ops:/ops:ro alpine/k8s:1.33.3 cat /ops/kubeconfig\n",
    );
    assert_eq!(
        unplaced(text, Shape::Shell),
        vec![
            (1, "--shm-size".to_string()),
            (2, "--stop-timeout".to_string())
        ],
        "an option in neither VALUED nor BOOLEAN must refuse its run by name, not read its value as the image"
    );
    assert_eq!(covered(text, Shape::Shell), vec![(3, false)]);
    let py = "    argv = ['sudo', 'docker', 'run', '--init', '--mount', 'type=bind,src=/k,dst=/k', 'img', 'cat']\n";
    assert_eq!(unplaced(py, Shape::Python), vec![(1, "--init".to_string())]);
}

#[test]
fn a_recovery_sheet_command_is_covered_and_its_patterns_are_not() {
    // Review faa40da8 finding 1: the sheet's typed lines, and the `with`
    // patterns beside them that merely spell a kubectl line to read.
    let toml = concat!(
        "command = \"sudo docker run --rm -it --network host -v {ops}/kubeconfig:/kc:ro {image} kubectl -n x exec -it deploy/d -- sh\"\n",
        "with = { image = { file = \"f.sh\", pattern = '^K=\"docker run .* (alpine/k8s:\\S+) kubectl ' } }\n",
        "command = \"sudo docker run --rm --log-driver=none --network host -v {ops}/kubeconfig:/kc:ro {image} kubectl delete x\"\n",
    );
    assert_eq!(
        covered(toml, Shape::TomlCommands),
        vec![(1, false), (3, true)]
    );
}

#[test]
fn every_covered_container_in_the_tree_keeps_no_log() {
    let root = repo_root();
    let mut found = Vec::new();
    files(&root.join("infra"), &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));

    let mut seen = 0usize;
    let mut sheet = 0usize;
    let mut logged = Vec::new();
    let mut unread = Vec::new();
    for (f, shape) in &found {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f.strip_prefix(&root).unwrap_or(f);
        for (line, option) in unplaced(&text, *shape) {
            unread.push(format!("{}:{line} {option}", rel.display()));
        }
        for (line, quiet) in covered(&text, *shape) {
            seen += 1;
            if rel == Path::new("infra/recovery/re-entry.toml") {
                sheet += 1;
            }
            if !quiet {
                logged.push(format!("{}:{line}", rel.display()));
            }
        }
    }

    // Floors, so a reader that stopped matching cannot pass on zero:
    // seventeen kubectl runs stood in the tree when this pin was
    // written, and the recovery sheet types four forge-side lines.
    assert!(
        seen >= 20,
        "found only {seen} covered container run(s) under infra/ — the reader no longer sees the tree's shape"
    );
    assert!(
        sheet >= 4,
        "found only {sheet} docker run line(s) on infra/recovery/re-entry.toml — the sheet reader no longer sees its commands"
    );
    assert!(
        unread.is_empty(),
        "{} container run(s) carry a docker option this pin cannot place, so it cannot tell the \
         option's value from the image or see any mount after it (backlog b21faf52). Add each \
         option to VALUED if it takes the next word as its value, or to BOOLEAN if it takes \
         none, in crates/core/boss-testing/tests/a_kubectl_container_keeps_no_log.rs — or \
         spell it `--option=value`:\n  {}",
        unread.len(),
        unread.join("\n  ")
    );
    assert!(
        logged.is_empty(),
        "{} container run(s) that run a cluster client or are handed a host mount keep a \
         container log, so whatever they print — a Secret, a mounted key's error, a recovery \
         session's terminal — is written by the docker daemon's log driver as well as read by \
         the caller (backlogs aa700921, ff957587). Add `{NO_LOG}` among docker's options, before \
         the image, to each:\n  {}",
        logged.len(),
        logged.join("\n  ")
    );
}
