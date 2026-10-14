//! A kubectl run in a container keeps no container log.
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
//! THE RULE: every `docker run` in the tree's shell that runs kubectl
//! (or the talosctl `ops_talosctl` mounts into the same image) carries
//! `--log-driver=none`. The docker CLI attaches to the
//! container's stdout before it starts it, and the attach stream is not
//! the log driver, so the script still reads every byte; nothing else
//! does. The rule covers EVERY kubectl container, not only the lines
//! that read a Secret today, because the kubectl line is shared: one
//! `ops_kubectl` serves every verb, and any later `get secret` through
//! any of these lines would otherwise inherit the log silently.
//!
//! tree-wide pin — it scans infra/, which no changed-file map
//! attributes to this crate, so every scoped gate runs it whatever its
//! scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// Built in halves so this file's own text is not an instance.
const NO_LOG: &str = concat!("--log-driver", "=none");
const DOCKER_RUN: &str = concat!("docker ", "run");

/// Every shell file under `dir`: `*.sh`, or a file whose first line is a
/// shebang (the pod's doors under infra/dev carry no extension).
fn shell_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            shell_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "sh")
            || std::fs::read_to_string(&p).is_ok_and(|t| t.starts_with("#!"))
        {
            out.push(p);
        }
    }
}

/// The logical commands of a shell text: physical lines joined across a
/// trailing backslash, each with the line number it starts on. A line
/// that is a comment is prose about the rule, never an instance of it.
fn commands(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        let (start, mut acc) = match cur.take() {
            Some(c) => c,
            None => {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                (i + 1, String::new())
            }
        };
        match line.trim_end().strip_suffix('\\') {
            Some(head) => {
                acc.push_str(head);
                acc.push(' ');
                cur = Some((start, acc));
            }
            None => {
                acc.push_str(line);
                out.push((start, acc));
            }
        }
    }
    out.extend(cur);
    out
}

/// A command that runs a cluster client in a container: kubectl, or the
/// talosctl `ops_talosctl` mounts into the same image (whose `get` of a
/// machine config would print the cluster's secrets the same way).
fn runs_kubectl_in_a_container(cmd: &str) -> bool {
    cmd.contains(DOCKER_RUN)
        && cmd
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .any(|w| w == "kubectl" || w == "talosctl")
}

/// Every kubectl container in `text`, with whether it keeps no log.
fn kubectl_runs(text: &str) -> Vec<(usize, bool)> {
    commands(text)
        .into_iter()
        .filter(|(_, c)| runs_kubectl_in_a_container(c))
        .map(|(n, c)| (n, c.contains(NO_LOG)))
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
        kubectl_runs(text),
        vec![(1, false), (2, false), (3, true), (4, false), (9, false)],
        "the reader must find the array, the quoted line, the printf, the continued run and the \
         talosctl — and neither the comment nor a run of anything but a cluster client"
    );
}

#[test]
fn every_kubectl_container_in_the_tree_keeps_no_log() {
    let root = repo_root();
    let mut files = Vec::new();
    shell_files(&root.join("infra"), &mut files);
    files.sort();

    let mut seen = 0usize;
    let mut logged = Vec::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for (line, quiet) in kubectl_runs(&text) {
            seen += 1;
            if !quiet {
                logged.push(format!(
                    "{}:{line}",
                    f.strip_prefix(&root).unwrap_or(f).display()
                ));
            }
        }
    }

    // A floor, so a reader that stopped matching cannot pass on zero:
    // seventeen such runs stood in the tree when this pin was written.
    assert!(
        seen >= 10,
        "found only {seen} kubectl container run(s) under infra/ — the reader no longer sees the tree's shape"
    );
    assert!(
        logged.is_empty(),
        "{} kubectl container run(s) keep a container log, so a Secret kubectl prints is written \
         by the docker daemon's log driver as well as read by the script (backlog aa700921). \
         Add `{NO_LOG}` to each:\n  {}",
        logged.len(),
        logged.join("\n  ")
    );
}
