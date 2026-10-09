//! The one namespace that admits a node capability holds ONE workload,
//! with exactly the pod and exactly the script its manifest shows — and
//! nothing else in the tree asks for what that namespace admits.
//!
//! WHY (backlog 17f6170c, 2026-10-07). `boss-node-maintenance` is the
//! first namespace this tree labels `pod-security.kubernetes.io/enforce:
//! privileged`, for the first workload that needs a node capability: a
//! daily `fstrim` of the build node's two filesystems, which takes
//! CAP_SYS_ADMIN and a descriptor on each filesystem. PodSecurity
//! `privileged` admits ANYTHING, so the label itself bounds nothing: a
//! second CronJob dropped into that file, a third hostPath, `privileged:
//! true`, a ServiceAccount token or a Secret mount would each be admitted
//! by the cluster without a word.
//!
//! WHERE THE BOUND IS NOW (the fold onto backlog 8eac4893, 2026-10-07).
//! AT ADMISSION: infra/cluster/manifests/boss-node-maintenance-admission.yaml
//! declares the Namespace, its `privileged` label, and a
//! ValidatingAdmissionPolicy that admits into it only the trim CronJob, a
//! Job and a Pod whose pod spec is the trim pod. That file landed first
//! and has its own pin, which EVALUATES the policy against this
//! manifest's CronJob
//! (`the_node_maintenance_namespace_is_bounded_at_admission.rs`). This
//! pin is the line-based net beside it: the early, local reading of a
//! diff, held to text. The trim manifest declares NO Namespace — the
//! converge applies the files that carry one first, and the policy must
//! be written before the CronJob — so the Namespace document this pin
//! holds is read from the admission file.
//!
//! WHAT THE POD IS. Its image and its script are root on w-1: with
//! CAP_SYS_ADMIN and no user namespace the default seccomp profile lets
//! it call mount, so hostile code in either could remount its read-only
//! directories read-write or reach the host. Nothing in the pod's
//! settings bounds that. The controls are the image's digest, the script
//! held below, and review of a change to either — which is why this pin
//! holds text, not intentions.
//!
//! WHAT IS HELD.
//!   * THE WHOLE FILE, AS LINES. Every line of the manifest that is not
//!     a comment, blank, or script body must equal, in order, the two
//!     documents below ([`POLICY_DOC`], [`TRIM_CRONJOB`]) each behind a
//!     bare `---`. A third object written as lines — behind a separator
//!     that carries a comment or a whole flow document, with a quoted
//!     key, after a directive — is a line that is not in the copy. And
//!     the Namespace document, in the admission file, must equal
//!     [`NAMESPACE_DOC`] the same way: its three labels and no fourth,
//!     the `privileged` line included.
//!   * THAT A LINE HERE IS A LINE TO KUBECTL. No yaml under `infra/`,
//!     this manifest included, may carry a carriage return, U+0085,
//!     U+2028 or U+2029 anywhere, comments included, or be anything but
//!     plain UTF-8: a YAML reader ends a line at each of those, so what
//!     follows one inside a `# comment` was a comment to this pin and a
//!     second container to kubectl (review 9b05cc55, B2a).
//!   * THE SCRIPT, BYTE FOR BYTE ([`TRIM_SCRIPT`]). It is the only code
//!     that runs with the capability, so one more line of it is a red
//!     naming that line. It is also RUN under stubs: it trims the two
//!     mounts, fails when either trim fails, refuses when both paths are
//!     one filesystem, and survives a termination log it cannot write.
//!     These are facts written twice on purpose: changing what runs
//!     with a node capability takes an edit here, where the diff is
//!     nothing but that change, as well as in the manifest.
//!   * THE REST OF THE TREE, BY BARE TOKEN ON A LINE. No other yaml
//!     under `infra/` may carry, on any line that is not a comment, the
//!     namespace's name, `hostPath`, `privileged`, `hostPID`,
//!     `hostNetwork`, `hostIPC` or `SYS_ADMIN` — as a substring, so a
//!     quoted value, a flow mapping or a JSON-style line is found the
//!     same as a block key — nor a `\u`/`\x` escape that could spell
//!     one of them. And no file of ANY kind under `infra/` names the
//!     namespace beyond the allow-list [`NAMES_THE_NAMESPACE`], line
//!     count and reason each. ONE FILE IS EXCEPTED FROM THE TOKENS, BY
//!     NAME ([`ADMISSION`]): the policy has to spell `hostPath`,
//!     `privileged`, `SYS_ADMIN`, the three host namespaces and the
//!     namespace's name to bound them, in CEL and in its refusal
//!     messages. It is excepted from nothing else here (the escape
//!     rule, the line breaks and the hidden spellings still read it),
//!     and what keeps a workload out of it is its own pin, which reads
//!     it with a YAML reader and holds it to exactly three objects.
//!   * THE SPELLINGS THAT TAKE A WORD OFF ITS LINE (review 9b05cc55,
//!     B3a), refused in every other yaml outside comment lines and the
//!     bodies of plain block scalars ([`hidden_spellings`]): a tag or a
//!     directive (`!!binary` decodes to a string no line contains); a
//!     line ending in a backslash, an odd count of double quotes, a
//!     quoted scalar left open (a double-quoted scalar continued by `\`
//!     joins its pieces with nothing between); an explicit key; flow
//!     brackets that do not balance on their line; a tab; a line ending
//!     in `|` or `>` that is not a plain header. Measured on origin/main
//!     e847d8a8 (2026-10-07) with this car merged: 0 lines of any of
//!     these, and 0 token lines, in the 33 other yaml files.
//!   * The two host directories are ones the tree declares elsewhere, so
//!     retiring either names this pod instead of leaving it unable to
//!     start with nobody told.
//!   * The hostPath lint, by running it against fixture trees: it
//!     admits the trim pod's two mounts and refuses a mount in a
//!     baseline namespace the fixture DECLARES, in an undeclared one, in
//!     a document that names no namespace, in flow style, and behind a
//!     separator that is not a bare `---`.
//!
//! WHAT IS NOT HELD, said rather than implied.
//!   * THE BOUND IS A LINE-BASED READING OF YAML, PLUS REVIEW. This is
//!     not a YAML reader and does not claim every spelling. It was
//!     considered and not chosen: the gate's image carries no kubectl
//!     and python3 without a yaml module, and a YAML crate added to
//!     this workspace for one pin would be a SECOND reader, not
//!     kubectl's — where the two disagree is one more place to hide.
//!     So the net is line-based, cut to fail closed (what it cannot
//!     place, it reports), and twice already a review has found a
//!     spelling it missed. The durable bound is at admission — the
//!     ValidatingAdmissionPolicy in [`ADMISSION`], item 8eac4893,
//!     landed — and the pod's own narrowing is the seccomp profile,
//!     item fb2f849f, still owed.
//!   * THIS FILE'S OWN REVIEW. The park hold covers `infra/` and
//!     `examples/`; a car that only loosens this pin touches neither
//!     and parks unheld. It cannot change what runs, but it can prepare
//!     a later manifest car (review 9b05cc55, N8). Holding it takes a
//!     change to the hold's own rules, which is not this car's.
//!   * Live facts: that the API server accepts the CronJob; that
//!     CAP_SYS_ADMIN without `privileged: true` is enough for FITRIM
//!     through a read-only bind mount on this kernel; that the Talos
//!     kubelet admits the two hostPath directories; that each directory
//!     is root-owned or other r-x (with `drop [ALL]` uid 0 has no DAC
//!     override, so otherwise the first run fails "Permission denied");
//!     that the CNI enforces the NetworkPolicy. The first run's
//!     termination message answers the first four.
//!   * What the IMAGE contains beyond its digest.
//!   * Code that creates a pod through kubectl rather than a manifest
//!     (a script under `infra/` holding the admin kubeconfig). It would
//!     have to name the namespace, which the allow-list catches; one
//!     that built the name from pieces would not be caught here.
//!   * Manifests outside `infra/`: the converge applies none.
//!
//! tree-wide pin — it reads every file under `infra/`, and a car that
//! adds a hostPath or a privileged label there maps to no crate, so a
//! scoped gate would skip the one check that refuses it
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{repo_root, scratch_dir};
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-node-maintenance.yaml";
const NAMESPACE: &str = "boss-node-maintenance";
/// The file that declares the Namespace and bounds it at admission. The
/// ONE other yaml that may carry [`TOKENS`], because a policy has to
/// spell what it bounds; named here and nowhere matched by pattern.
const ADMISSION: &str = "infra/cluster/manifests/boss-node-maintenance-admission.yaml";

/// The Namespace document, whole — as [`ADMISSION`] declares it. Its
/// last line is the privilege grant.
const NAMESPACE_DOC: &str = r#"apiVersion: v1
kind: Namespace
metadata:
  name: boss-node-maintenance
  labels:
    app.kubernetes.io/part-of: boss
    boss.dev/purpose: node-maintenance
    pod-security.kubernetes.io/enforce: privileged"#;

/// The NetworkPolicy document, whole: every pod, nothing either way.
const POLICY_DOC: &str = r#"apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: deny-all
  namespace: boss-node-maintenance
  labels:
    app.kubernetes.io/part-of: boss
    boss.dev/purpose: node-maintenance
spec:
  podSelector: {}
  policyTypes:
    - Ingress
    - Egress"#;

/// The CronJob, comments, blank lines and the body of its inline script
/// removed. See the module header for why this is a second copy.
const TRIM_CRONJOB: &str = r#"apiVersion: batch/v1
kind: CronJob
metadata:
  name: boss-node-trim
  namespace: boss-node-maintenance
  labels:
    app.kubernetes.io/part-of: boss
    boss.dev/purpose: node-maintenance
spec:
  schedule: "40 12 * * *"
  concurrencyPolicy: Forbid
  startingDeadlineSeconds: 3600
  successfulJobsHistoryLimit: 3
  failedJobsHistoryLimit: 3
  jobTemplate:
    spec:
      backoffLimit: 0
      activeDeadlineSeconds: 1800
      template:
        metadata:
          labels:
            app: boss-node-trim
        spec:
          restartPolicy: Never
          automountServiceAccountToken: false
          enableServiceLinks: false
          hostNetwork: false
          hostPID: false
          hostIPC: false
          nodeSelector:
            boss.dev/purpose: build
          securityContext:
            runAsNonRoot: false
            runAsUser: 0
            runAsGroup: 0
            seccompProfile:
              type: RuntimeDefault
          containers:
            - name: trim
              image: 10.20.0.15:3000/david/alpine-k8s:1.33.3@sha256:cdeda0da2cd6896023cc5f96505bc94a699a989dff2bfc3e2008ab92eab1e93a
              securityContext:
                privileged: false
                readOnlyRootFilesystem: true
                capabilities:
                  drop:
                    - ALL
                  add:
                    - SYS_ADMIN
              resources:
                requests:
                  cpu: 10m
                  memory: 16Mi
                limits:
                  cpu: 500m
                  memory: 64Mi
              volumeMounts:
                - name: gate
                  mountPath: /trim/gate
                  readOnly: true
                - name: ephemeral
                  mountPath: /trim/ephemeral
                  readOnly: true
              command:
                - /bin/sh
                - -c
              args:
                - |
          volumes:
            - name: gate
              hostPath:
                path: /var/mnt/gate/seed
                type: Directory
            - name: ephemeral
              hostPath:
                path: /var/local/gate-seed
                type: Directory"#;

/// The inline script, de-indented, byte for byte: the only code that
/// runs with the capability. A reviewer of a change to it reads the
/// diff of these lines, not a hash.
const TRIM_SCRIPT: &str = r##"set -u
# The four paths are parameters only so the pin can RUN
# this text (inline shell nothing executes is untested
# shell); the pod sets no env, so it gets the defaults.
GATE="${TRIM_GATE:-/trim/gate}"
EPHEMERAL="${TRIM_EPHEMERAL:-/trim/ephemeral}"
OUT="${TRIM_TERMINATION_LOG:-/dev/termination-log}"
SYS="${TRIM_SYS_BLOCK:-/sys/block}"
# Every line goes to the log AND the termination message,
# so the result is on the pod's status as well as in a
# log that needs a second grant to read.
say() {
  echo "node-trim: $*"
  echo "node-trim: $*" >>"$OUT" 2>/dev/null || true
}
# The EFFECT, beside the claim: each NVMe's completed
# discards and discarded sectors (fields 12 and 14 of
# its stat), before and after.
counters() {
  for s in "$SYS"/nvme*/stat; do
    [ -r "$s" ] || continue
    say "$1 ${s%/stat}: $(awk '{print "discards=" $12 " sectors=" $14}' "$s")"
  done
  return 0
}
# A gate volume that is not mounted leaves its directory
# on EPHEMERAL: then both paths are one filesystem and
# the drive this exists for would go untrimmed in
# silence. Refuse, by name.
gd=$(stat -c %d "$GATE") || { say "RED cannot stat $GATE"; exit 1; }
ed=$(stat -c %d "$EPHEMERAL") || { say "RED cannot stat $EPHEMERAL"; exit 1; }
if [ "$gd" = "$ed" ]; then
  say "RED $GATE and $EPHEMERAL are one filesystem (device $gd): the gate volume is not mounted, nothing trimmed"
  exit 1
fi
counters before
rc=0
for m in "$GATE" "$EPHEMERAL"; do
  if out=$(fstrim -v "$m" 2>&1); then
    say "$out"
  else
    say "RED fstrim $m failed: $out"
    rc=1
  fi
done
counters after
if [ "$rc" -eq 0 ]; then say "OK"; else say "RED"; fi
exit "$rc""##;

/// Every file under `infra/` that may name the namespace, other than
/// the manifest: (path, lines that name it, why). A line more in any of
/// them, or a file not listed, is a finding — `instances.toml` naming it
/// would render a whole instance into the privileged namespace.
const NAMES_THE_NAMESPACE: &[(&str, usize, &str)] = &[
    (
        "infra/cluster/instance-manifests.txt",
        2,
        "the two roster lines that classify the manifest and the admission file `pipeline`",
    ),
    (
        "infra/cluster/manifests/README.md",
        2,
        "the directory's rule paragraph about the one privileged namespace, naming the two files",
    ),
    (
        "infra/cluster/undeclared-objects.sh",
        1,
        "a comment listing the namespaces the tree owns",
    ),
    (
        "infra/forge/check-node-trim.sh",
        1,
        "the READER, from outside (backlog 33e62de8): `NS=`, the one namespace its `kubectl get` \
         and `kubectl logs` are aimed at through the forge's admin kubeconfig. It sends no \
         write — node_trim_check_sh.rs holds every door call to get or logs, and the verb \
         takes no argument",
    ),
    (
        "infra/lint/a-workload-declares-the-user-it-runs-as.sh",
        1,
        "the EXEMPT entry for the manifest's declared uid 0",
    ),
    (
        "infra/forge/trim-build-node.sh",
        3,
        "the ops verb that starts the DECLARED trim now (backlog 30a6bf39): the admission \
         file's path in its header, the namespace as the literal `NS`, and the manifest it \
         reads the declared image, script and deadline from. It carries no pod spec — it \
         makes a Job `--from` the CronJob — and trim_build_node_sh.rs holds it to that",
    ),
    (
        "infra/ops/verbs/trim-build-node.json",
        1,
        "that verb's `about`, one line: the namespace its script creates the Job in",
    ),
    (
        "infra/lint/no-manifest-mounts-a-hostpath.sh",
        3,
        "a header comment naming the manifest that made the lint learn a namespace, and the \
         admission file's path twice: in the header and as POLICY_FILE, the one file whose \
         policy text may say `hostPath`",
    ),
    (
        "infra/lint/the-estate-address-lives-once.sh",
        1,
        "the allowance for the manifest's image line",
    ),
    (
        "infra/ops/verbs/check-node-trim.json",
        1,
        "the READ-ONLY verb's `about`, which says which namespace check-node-trim.sh reads",
    ),
];

/// What no other yaml under `infra/` may carry on a non-comment line.
const TOKENS: &[&str] = &[
    NAMESPACE,
    "hostPath",
    "privileged",
    "hostPID",
    "hostNetwork",
    "hostIPC",
    "SYS_ADMIN",
];

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn lines_of(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// A manifest's documents, each as its lines. YAML starts a document at
/// ANY line beginning `---` — bare, or carrying a comment or content —
/// so that is the split (review 16efea28, B2: a split on the bare line
/// alone folded `--- # x` and the object after it into the document
/// before). What follows the dashes on that line opens the new document.
fn docs(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .fold(vec![Vec::new()], |mut acc: Vec<Vec<String>>, l| {
            match l.strip_prefix("---") {
                Some(rest) => acc.push(vec![rest.trim_start().to_string()]),
                None => acc.last_mut().unwrap().push(l.to_string()),
            }
            acc
        })
        .into_iter()
        .filter(|d| {
            d.iter()
                .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        })
        .collect()
}

/// Lines with comment lines, blank lines and every block scalar's body
/// removed: what Kubernetes is told, minus the one script.
fn skeleton(doc: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut scalar_at: Option<usize> = None;
    for l in doc {
        if let Some(depth) = scalar_at {
            if l.trim().is_empty() || indent(l) > depth {
                continue;
            }
            scalar_at = None;
        }
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        if l.trim_end().ends_with(": |") || l.trim_end().ends_with("- |") {
            scalar_at = Some(indent(l));
        }
        out.push(l.trim_end().to_string());
    }
    out
}

/// The body of the one block scalar in a document, de-indented.
fn block_scalar(doc: &[String]) -> String {
    let start = doc
        .iter()
        .position(|l| l.trim_end().ends_with("- |"))
        .expect("the CronJob carries its script as a block scalar");
    let depth = indent(&doc[start]);
    let body: Vec<&String> = doc[start + 1..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent(l) > depth)
        .collect();
    let strip = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .min()
        .expect("the script is not empty");
    body.iter()
        .map(|l| if l.len() >= strip { &l[strip..] } else { "" })
        .collect::<Vec<_>>()
        .join("\n")
}

fn top(doc: &[String], key: &str) -> String {
    let p = format!("{key}: ");
    doc.iter()
        .find_map(|l| l.strip_prefix(&p))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn trim_cronjob() -> Vec<String> {
    docs(&read(MANIFEST))
        .into_iter()
        .find(|d| top(d, "kind") == "CronJob")
        .expect("the manifest declares the trim CronJob")
}

/// `live` against `want`, line for line: `None` when equal, else the
/// 1-based line and the two sides.
fn first_difference(live: &[String], want: &[&str]) -> Option<(usize, String, String)> {
    let side = |v: Option<&str>| v.map_or_else(|| "(no line)".to_string(), |s| format!("{s:?}"));
    (0..live.len().max(want.len()))
        .find(|&i| live.get(i).map(String::as_str) != want.get(i).copied())
        .map(|i| {
            (
                i + 1,
                side(live.get(i).map(String::as_str)),
                side(want.get(i).copied()),
            )
        })
}

/// Every file under `infra/` with one of `exts` (or every file when
/// `exts` is empty), as (path relative to the repo, text). A file that
/// is not UTF-8 is read lossily: a token in it is still a token.
fn infra_files(exts: &[&str]) -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, exts: &[&str], out: &mut Vec<(String, String)>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, root, exts, out);
            } else if exts.is_empty()
                || p.extension()
                    .is_some_and(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e)))
            {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                let bytes =
                    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
                out.push((rel, String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("infra"), &root, exts, &mut out);
    out
}

#[test]
fn the_namespace_holds_exactly_these_objects() {
    let live = skeleton(&lines_of(&read(MANIFEST)));
    let want: Vec<&str> = [POLICY_DOC, TRIM_CRONJOB]
        .iter()
        .flat_map(|d| std::iter::once("---").chain(d.lines()))
        .collect();
    let diff = first_difference(&live, &want);
    assert!(
        diff.is_none(),
        "{MANIFEST} is not, line for line, the deny-all NetworkPolicy and ONE CronJob, each \
         behind a bare `---` (comments and the script's body aside) — and no Namespace: that \
         is declared in {ADMISSION}, so the converge writes the policy before this CronJob.\n  \
         first difference at non-comment line {}:\n  manifest: {}\n  pin:      {}\n\
         PodSecurity `privileged` admits anything, so an object added here is admitted \
         with a node capability in reach and nothing in the cluster says so. It gets its own car and its own review \
         (backlog 17f6170c; review 16efea28, B2).",
        diff.as_ref().map_or(0, |d| d.0),
        diff.as_ref().map_or("", |d| d.1.as_str()),
        diff.as_ref().map_or("", |d| d.2.as_str()),
    );
    let held: Vec<String> = docs(&read(MANIFEST))
        .iter()
        .map(|d| top(d, "kind"))
        .collect();
    assert_eq!(
        held,
        ["NetworkPolicy", "CronJob"],
        "and it splits into exactly those two documents"
    );
}

/// The Namespace is declared ONCE, in the admission file, and it is
/// exactly [`NAMESPACE_DOC`]: three labels, the last of them the
/// privilege grant. A second pod-security label (`warn`, `audit`, an
/// `enforce-version`), a fourth label or a second Namespace document is
/// a line that is not in the copy. That file's own pin holds the kinds
/// with a YAML reader; this holds the lines.
#[test]
fn the_namespace_is_declared_in_the_admission_file_exactly_so() {
    let declared: Vec<Vec<String>> = docs(&read(ADMISSION))
        .into_iter()
        .filter(|d| top(d, "kind") == "Namespace")
        .collect();
    assert_eq!(declared.len(), 1, "{ADMISSION} declares the Namespace once");
    let live = skeleton(&declared[0]);
    let want: Vec<&str> = NAMESPACE_DOC.lines().collect();
    let diff = first_difference(&live, &want);
    assert!(
        diff.is_none(),
        "{ADMISSION}: the Namespace document differs from the one this pin holds it to \
         (line {} of the document, comments aside).\n  manifest: {}\n  pin:      {}\n\
         Its `enforce` line is what admits a node capability into {NAMESPACE}; the policy in \
         the same file is what bounds it. A change to either label is its own car and its \
         own review (backlog 17f6170c).",
        diff.as_ref().map_or(0, |d| d.0),
        diff.as_ref().map_or("", |d| d.1.as_str()),
        diff.as_ref().map_or("", |d| d.2.as_str()),
    );
    // And it is the LAST document of that file, so `docs` did not read
    // another object's lines into it.
    assert_eq!(
        docs(&read(ADMISSION))
            .iter()
            .map(|d| top(d, "kind"))
            .collect::<Vec<_>>(),
        [
            "ValidatingAdmissionPolicy",
            "ValidatingAdmissionPolicyBinding",
            "Namespace"
        ],
    );
    // The trim manifest carries no Namespace line at all: the converge
    // picks its namespace-phase files by that exact line.
    assert!(
        !read(MANIFEST)
            .lines()
            .any(|l| l.trim() == "kind: Namespace"),
        "{MANIFEST} carries a `kind: Namespace` line: cluster-deploy-runner.sh would apply \
         the whole file, CronJob included, in its namespace phase — before the policy"
    );
}

#[test]
fn the_trim_pod_is_exactly_this_pod() {
    let live = skeleton(&trim_cronjob());
    let want: Vec<&str> = TRIM_CRONJOB.lines().collect();
    let diff = first_difference(&live, &want);
    assert!(
        diff.is_none(),
        "{MANIFEST}: the trim CronJob differs from the pod this pin holds it to \
         (line {} of the document, comments and script aside).\n  manifest: {}\n  pin:      {}\n\
         This pod runs as root with CAP_SYS_ADMIN and two host mounts in a namespace \
         that admits anything. If the change is meant, make it in TRIM_CRONJOB too, in a \
         car that says why (backlog 17f6170c).",
        diff.as_ref().map_or(0, |d| d.0),
        diff.as_ref().map_or("", |d| d.1.as_str()),
        diff.as_ref().map_or("", |d| d.2.as_str()),
    );
    assert!(
        !trim_cronjob()
            .iter()
            .filter(|l| !l.trim_start().starts_with('#'))
            .any(|l| l.contains("boss-chore")),
        "the converge rolls every `boss-chore=true` CronJob's `chore` container to the \
         image it just built (cluster-deploy-runner.sh); this pod's image is pinned by \
         digest and must never be on that path"
    );
}

#[test]
fn the_script_is_exactly_this_script() {
    let live = lines_of(&block_scalar(&trim_cronjob()));
    let want: Vec<&str> = TRIM_SCRIPT.lines().collect();
    let diff = first_difference(&live, &want);
    assert!(
        diff.is_none(),
        "{MANIFEST}: the inline script differs from the text this pin holds it to, at line \
         {} of the script.\n  manifest: {}\n  pin:      {}\n\
         This script is the only code that runs as root with CAP_SYS_ADMIN on w-1, and \
         with that capability a line of shell can remount the gate seed read-write or \
         reach the host. A change to it — a comment included — is made in TRIM_SCRIPT too, \
         where the diff is nothing but that change, in a car that is reviewed for it \
         (backlog 17f6170c; review 16efea28, B1).",
        diff.as_ref().map_or(0, |d| d.0),
        diff.as_ref().map_or("", |d| d.1.as_str()),
        diff.as_ref().map_or("", |d| d.2.as_str()),
    );
}

/// The script, run. `fstrim` and `stat` are stubs on PATH: the stub
/// fstrim prints BusyBox's own line and fails for a path named in
/// FSTRIM_FAIL; the stub stat answers a device number per path.
fn run_script(
    case: &str,
    same_device: bool,
    fail: &str,
    log_writable: bool,
) -> (i32, String, String) {
    let dir = scratch_dir(&format!("node-trim-{case}"));
    let bin = dir.join("bin");
    let sys = dir.join("sys/nvme9n1");
    for d in [&bin, &sys, &dir.join("gate"), &dir.join("ephemeral")] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        sys.join("stat"),
        "1 2 3 4 5 6 7 8 9 10 11 699 13 4096 15 16 17\n",
    )
    .unwrap();
    let stub = |name: &str, body: &str| {
        let p = bin.join(name);
        boss_testing::write_exec(&p, body);
    };
    stub(
        "fstrim",
        "#!/bin/sh\n[ \"$1\" = -v ] || { echo \"fstrim: unexpected args: $*\" >&2; exit 2; }\n\
         case \"$2\" in *\"$FSTRIM_FAIL\") echo \"fstrim: $2: FITRIM: Operation not permitted\" >&2; exit 1;; esac\n\
         echo \"$2: 123456789 bytes trimmed\"\n",
    );
    stub(
        "stat",
        &format!(
            "#!/bin/sh\ncase \"$3\" in */gate) echo 64769;; *) echo {};; esac\n",
            if same_device { 64769 } else { 64770 }
        ),
    );
    let script = dir.join("trim.sh");
    std::fs::write(&script, block_scalar(&trim_cronjob())).unwrap();
    let log = if log_writable {
        dir.join("termination-log")
    } else {
        dir.join("no-such-dir/termination-log")
    };
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("sh")
        .arg(&script)
        .env("PATH", path)
        .env("TRIM_GATE", dir.join("gate"))
        .env("TRIM_EPHEMERAL", dir.join("ephemeral"))
        .env("TRIM_TERMINATION_LOG", &log)
        .env("TRIM_SYS_BLOCK", dir.join("sys"))
        .env(
            "FSTRIM_FAIL",
            if fail.is_empty() {
                "/nothing-fails"
            } else {
                fail
            },
        )
        .output()
        .expect("sh runs the script");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    (out.status.code().unwrap_or(-1), said, logged)
}

#[test]
fn the_script_trims_both_mounts_and_says_what_it_did() {
    let (rc, said, logged) = run_script("ok", false, "", true);
    assert_eq!(rc, 0, "both trims succeeded:\n{said}");
    for want in [
        "/gate: 123456789 bytes trimmed",
        "/ephemeral: 123456789 bytes trimmed",
        "node-trim: before ",
        "node-trim: after ",
        "discards=699 sectors=4096",
        "node-trim: OK",
    ] {
        assert!(said.contains(want), "stdout says `{want}`:\n{said}");
        assert!(
            logged.contains(want),
            "the termination message says `{want}` too — it is the copy readable off the \
             pod's status, without a grant on logs:\n{logged}"
        );
    }
    assert!(!said.contains("RED"), "nothing failed:\n{said}");
    assert!(
        logged.len() < 4096,
        "the kubelet keeps 4096 bytes of a termination message; this is {}",
        logged.len()
    );
}

#[test]
fn a_failed_trim_fails_the_job_and_still_trims_the_other_mount() {
    let (rc, said, logged) = run_script("gate-fails", false, "/gate", true);
    assert_ne!(
        rc, 0,
        "a failed trim is a failed Job, never a quiet one:\n{said}"
    );
    assert!(
        said.contains("RED fstrim") && said.contains("/gate failed") && said.contains("FITRIM"),
        "the line names the mount and carries what fstrim said:\n{said}"
    );
    assert!(
        said.contains("/ephemeral: 123456789 bytes trimmed"),
        "one mount failing does not skip the other:\n{said}"
    );
    assert!(
        logged.trim_end().ends_with("node-trim: RED") && !logged.contains("node-trim: OK"),
        "the closing line is the verdict:\n{logged}"
    );
}

#[test]
fn one_filesystem_under_both_paths_is_refused() {
    let (rc, said, _) = run_script("same-device", true, "", true);
    assert_ne!(rc, 0, "{said}");
    assert!(
        said.contains("one filesystem") && !said.contains("bytes trimmed"),
        "an unmounted gate volume leaves its directory on EPHEMERAL; trimming that twice \
         and saying OK is the silent miss this refuses:\n{said}"
    );
}

#[test]
fn an_unwritable_termination_log_does_not_fail_the_trim() {
    let (rc, said, logged) = run_script("no-log", false, "", false);
    assert_eq!(
        rc, 0,
        "reporting is best-effort, the trim is the work:\n{said}"
    );
    assert!(
        said.contains("node-trim: OK") && logged.is_empty(),
        "{said}"
    );
}

/// Whether a line carries a `\u`, `\U` or `\x` escape with a hex digit
/// after it — the way a double-quoted YAML scalar could spell a token
/// this pin looks for without containing it.
fn carries_a_hex_escape(l: &str) -> bool {
    let b = l.as_bytes();
    b.windows(3)
        .any(|w| w[0] == b'\\' && matches!(w[1], b'u' | b'U' | b'x') && w[2].is_ascii_hexdigit())
}

#[test]
fn nothing_else_in_the_tree_asks_for_what_the_namespace_admits() {
    let yaml = infra_files(&["yaml", "yml"]);
    assert!(
        yaml.len() >= 25,
        "only {} yaml file(s) under infra/ — the walk lost its subject",
        yaml.len()
    );
    // BARE TOKENS, on any line that is not wholly a comment. Not a key
    // at the start of a line and not an exact `namespace: <name>`: a
    // quoted value, a flow mapping or a trailing-comment trick all
    // carry the token as a substring (review 16efea28, B3 — mutants 18
    // and 20 passed the prefix match). A trailing comment is NOT
    // stripped first: `{"a #": 1, privileged: true}` would lose its
    // second key to a naive strip.
    let mut findings = Vec::new();
    for (file, text) in &yaml {
        if file == MANIFEST {
            continue;
        }
        // THE ONE EXCEPTION, BY NAME: the admission policy spells these
        // words to refuse them. Only the tokens are excused; the escape
        // rule below still reads every one of its lines.
        let spells_what_it_bounds = file == ADMISSION;
        for (n, l) in text.lines().enumerate() {
            if l.trim_start().starts_with('#') {
                continue;
            }
            let hit: Vec<&str> = TOKENS
                .iter()
                .copied()
                .filter(|t| !spells_what_it_bounds && l.contains(t))
                .collect();
            if !hit.is_empty() {
                findings.push(format!(
                    "{file}:{}: [{}] {}",
                    n + 1,
                    hit.join(", "),
                    l.trim()
                ));
            }
            if carries_a_hex_escape(l) {
                findings.push(format!(
                    "{file}:{}: [a \\u or \\x escape, which can spell any of them] {}",
                    n + 1,
                    l.trim()
                ));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "only {MANIFEST} may place an object in {NAMESPACE} or declare a hostPath, a host \
         namespace, a privileged container or CAP_SYS_ADMIN, and only {ADMISSION} may label \
         that namespace privileged and spell those words in its policy \
         (backlog 17f6170c). Another yaml under infra/ carries one of those words on a line \
         that is not a comment — quoted, flow-style or plain: the check is the bare token \
         on the line. If it is prose, move it into a comment; if it is a workload, it is its own \
         car and its own review. Found:\n  {}",
        findings.join("\n  ")
    );

    // AND THE NAME ITSELF, in every file of every kind: an instance
    // declared into that namespace (instances.toml), or a script that
    // creates a pod there, names it without being yaml.
    let mut wrong = Vec::new();
    for (file, text) in infra_files(&[]) {
        // The two files that ARE the namespace: its workload, and its
        // declaration and bound. Each is held by its own pin.
        if file == MANIFEST || file == ADMISSION {
            continue;
        }
        let n = text.lines().filter(|l| l.contains(NAMESPACE)).count();
        let allowed = NAMES_THE_NAMESPACE
            .iter()
            .find(|(f, _, _)| *f == file)
            .map_or(0, |(_, n, _)| *n);
        if n != allowed {
            wrong.push(format!(
                "{file}: {n} line(s) name it, the allow-list says {allowed}"
            ));
        }
    }
    for (file, _, why) in NAMES_THE_NAMESPACE {
        assert!(
            repo_root().join(file).is_file(),
            "NAMES_THE_NAMESPACE lists {file} ({why}), which is gone — a dead entry would \
             admit the next file to take that name"
        );
    }
    assert!(
        wrong.is_empty(),
        "a file under infra/ names `{NAMESPACE}` that the allow-list does not account for. \
         Anything that can put an object in that namespace has to name it; add the file to \
         NAMES_THE_NAMESPACE with the reason, in a car reviewed for it:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_host_directories_are_ones_the_tree_declares() {
    // The gate volume's anchor: the PV's path plus the subPath the gate
    // runner mounts its seed at.
    assert!(
        read("infra/cluster/manifests/gate-seed-local.yaml").contains("    path: /var/mnt/gate\n"),
        "gate-seed-local.yaml no longer declares a PV at /var/mnt/gate — the trim pod's \
         hostPath /var/mnt/gate/seed names a directory under it ({MANIFEST})"
    );
    assert!(
        read("infra/gate-runner/gate-runner.yaml")
            .contains("{name: gate-disk, mountPath: /gate-seed, subPath: seed}"),
        "the gate runner no longer mounts subPath `seed` of the gate volume — that mount \
         is what makes /var/mnt/gate/seed exist for the trim pod ({MANIFEST})"
    );
    // EPHEMERAL's anchor: the directory w-1's Talos config creates and
    // binds into the kubelet.
    let w1 = read("infra/cluster/talos/patches/w-1.yaml");
    assert!(
        w1.contains("path: /var/local/gate-seed/.declared-by-talos")
            && w1.contains("- destination: /var/local/gate-seed"),
        "w-1's Talos patch no longer creates /var/local/gate-seed and binds it into the \
         kubelet — the trim pod reaches EPHEMERAL through that directory, and with \
         `type: Directory` it would stop starting ({MANIFEST}). Retire them together."
    );
}

#[test]
fn the_roster_applies_it_once_for_the_pipeline() {
    let roster = read("infra/cluster/instance-manifests.txt");
    assert!(
        roster
            .lines()
            .any(|l| l.trim() == "boss-node-maintenance.yaml pipeline"),
        "one physical node, one trim: rendered per instance it would put a second \
         privileged namespace's worth of objects wherever the playground renders"
    );
}

/// A fixture tree holding one lint, its library and a manifests
/// directory, so the lint is RUN against a tree that should fail. The
/// real manifest is copied in first, and the admission file with it —
/// that is where the namespace is labelled privileged, so without it the
/// trim pod's own mounts are refused; an entry of the same name replaces
/// either.
fn lint_fixture(case: &str, lint: &str, manifests: &[(&str, &str)]) -> (i32, String) {
    let root = repo_root();
    let dir = scratch_dir(&format!("privileged-ns-lint-{case}"));
    for d in [
        "infra/lint/lib",
        "infra/cluster/manifests",
        "infra/gate-runner",
    ] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    for f in [lint, "infra/lint/lib/scanned.sh"] {
        std::fs::copy(root.join(f), dir.join(f)).unwrap();
    }
    for f in [MANIFEST, ADMISSION] {
        std::fs::copy(root.join(f), dir.join(f)).unwrap();
    }
    for (name, text) in manifests {
        std::fs::write(dir.join("infra/cluster/manifests").join(name), text).unwrap();
    }
    let out = Command::new("bash")
        .arg(dir.join(lint))
        .output()
        .expect("bash runs the lint");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const HOSTPATH_LINT: &str = "infra/lint/no-manifest-mounts-a-hostpath.sh";

fn pod_with_hostpath(namespace: &str) -> String {
    format!(
        "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: rider\n  namespace: {namespace}\n\
         spec:\n  schedule: \"0 0 * * *\"\n  jobTemplate:\n    spec:\n      template:\n        spec:\n          \
         containers:\n            - name: c\n              image: x\n          volumes:\n            - name: host\n              \
         hostPath:\n                path: /\n"
    )
}

fn namespace_labelled(name: &str, level: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {name}\n  labels:\n    \
         pod-security.kubernetes.io/enforce: {level}\n"
    )
}

/// A DaemonSet written the way the reviewer's mutants were: flow style,
/// a quoted `kind`, a host mount of `/`.
fn flow_rider(metadata: &str) -> String {
    format!(
        "apiVersion: apps/v1\n\"kind\": DaemonSet\nmetadata: {metadata}\nspec:\n  \
         selector: {{matchLabels: {{app: rider}}}}\n  template:\n    metadata: {{labels: {{app: rider}}}}\n    \
         spec:\n      containers: [{{name: c, image: busybox, volumeMounts: [{{name: host, mountPath: /host}}]}}]\n      \
         volumes: [{{name: host, hostPath: {{path: /}}}}]\n"
    )
}

#[test]
fn the_hostpath_lint_admits_the_privileged_namespace_and_nothing_else() {
    let (rc, said) = lint_fixture("tree", HOSTPATH_LINT, &[]);
    assert_eq!(rc, 0, "the trim pod's two mounts are admitted:\n{said}");
    assert!(
        said.contains("2 hostPath volume(s)") && said.contains(NAMESPACE),
        "and the lint says how many it admitted, and where:\n{said}"
    );

    // The case the lint was written for: a hostPath in a baseline
    // namespace is a pod that is never admitted (d42d4967).
    let (rc, said) = lint_fixture(
        "baseline",
        HOSTPATH_LINT,
        &[("rider.yaml", &pod_with_hostpath("boss-dev"))],
    );
    assert_eq!(rc, 1, "{said}");
    assert!(said.contains("rider.yaml"), "it names the file:\n{said}");

    // …and in a baseline namespace the fixture DECLARES. Without the
    // declaration the leg above passes for the wrong reason — the
    // namespace is simply unknown — and a lint changed to read
    // `baseline` as privileged still passed it (review 16efea28, N5,
    // mutant 27).
    let declared = format!(
        "{}---\n{}",
        namespace_labelled("quiet", "baseline"),
        pod_with_hostpath("quiet")
    );
    let (rc, said) = lint_fixture(
        "declared-baseline",
        HOSTPATH_LINT,
        &[("rider.yaml", &declared)],
    );
    assert_eq!(
        rc, 1,
        "a namespace labelled baseline admits no hostPath:\n{said}"
    );
    assert!(
        said.contains("rider.yaml") && !said.contains("quiet "),
        "and `quiet` is not listed among the privileged namespaces:\n{said}"
    );

    // A document that does not say which namespace it is in is not
    // given the benefit of the doubt.
    let anonymous = pod_with_hostpath("x").replace("  namespace: x\n", "");
    let (rc, said) = lint_fixture("no-namespace", HOSTPATH_LINT, &[("rider.yaml", &anonymous)]);
    assert_eq!(rc, 1, "{said}");

    // And a file that labels its OWN namespace privileged to admit its
    // own mount passes this lint by design — the lint learns the
    // namespace from the declaration — which is exactly why
    // `nothing_else_in_the_tree_asks_for_what_the_namespace_admits`
    // exists beside it.
    let own = format!(
        "{}---\n{}",
        namespace_labelled("second", "privileged"),
        pod_with_hostpath("second")
    );
    let (rc, said) = lint_fixture("second-namespace", HOSTPATH_LINT, &[("rider.yaml", &own)]);
    assert_eq!(rc, 0, "{said}");
    assert!(
        said.contains("second"),
        "it names every namespace it admitted:\n{said}"
    );
}

/// The fold onto the admission policy (backlog 8eac4893): the policy's
/// CEL and its refusal message say `hostPath`, and the lint excuses that
/// word in ONE file's ValidatingAdmissionPolicy document, by name. Each
/// leg is a way that excuse could have been cut too wide.
#[test]
fn the_hostpath_lint_excuses_only_the_policys_own_text() {
    let admission = read(ADMISSION);
    let name = "boss-node-maintenance-admission.yaml";
    assert!(
        admission
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains("hostPath"))
            .count()
            >= 3,
        "the policy no longer says the word, so this test excuses nothing"
    );

    // The label is READ from that file: without it the trim pod's own
    // two mounts are in a namespace nothing labels privileged.
    let grant = "    pod-security.kubernetes.io/enforce: privileged\n";
    assert!(admission.ends_with(grant), "the grant is the last line");
    let unlabelled = admission.replacen(
        grant,
        "    pod-security.kubernetes.io/enforce: baseline\n",
        1,
    );
    let (rc, said) = lint_fixture(
        "policy-file-baseline",
        HOSTPATH_LINT,
        &[(name, &unlabelled)],
    );
    assert_eq!(
        rc, 1,
        "a baseline namespace admits no hostPath, the trim pod's included:\n{said}"
    );
    assert!(said.contains("boss-node-maintenance.yaml:"), "{said}");

    // A workload appended to the policy's file is not policy text: in
    // flow style behind a commented separator, and in block style.
    let flow = format!(
        "{admission}--- # one more\n{}",
        flow_rider("{name: rider, namespace: boss-node-maintenance}")
    );
    let (rc, said) = lint_fixture("policy-file-flow-rider", HOSTPATH_LINT, &[(name, &flow)]);
    assert_eq!(rc, 1, "{said}");
    assert!(
        said.contains(name) && said.contains("hostPath: {path: /}"),
        "it names the file and the mount:\n{said}"
    );
    let block = format!("{admission}---\n{}", pod_with_hostpath("boss-dev"));
    let (rc, said) = lint_fixture("policy-file-block-rider", HOSTPATH_LINT, &[(name, &block)]);
    assert_eq!(rc, 1, "{said}");
    assert!(said.contains(name), "{said}");

    // And the excuse is the FILE's, not the kind's: another file calling
    // its document a ValidatingAdmissionPolicy is read like any other.
    let pretender = "apiVersion: admissionregistration.k8s.io/v1\nkind: ValidatingAdmissionPolicy\n\
                     metadata:\n  name: pretender\nspec:\n  volumes: [{name: host, hostPath: {path: /}}]\n";
    let (rc, said) = lint_fixture(
        "policy-kind-elsewhere",
        HOSTPATH_LINT,
        &[("rider.yaml", pretender)],
    );
    assert_eq!(rc, 1, "{said}");
    assert!(said.contains("rider.yaml"), "{said}");
}

/// Review 16efea28, B2: the lint's reader split documents on a bare
/// `---` only and matched `hostPath:` at the start of a line only, so a
/// flow-style object behind `--- # comment` was read as part of the
/// document before it and its mount never seen.
#[test]
fn the_hostpath_lint_reads_every_document_in_every_style() {
    let real = read(MANIFEST);
    let before_policy = "---\napiVersion: networking.k8s.io/v1\n";
    assert!(
        real.contains(before_policy),
        "the fixture's insertion point"
    );

    // The reviewer's mutant 21, as it was written.
    let smuggled = real.replacen(
        before_policy,
        &format!(
            "--- # a second object, after the Namespace\n{}{before_policy}",
            flow_rider("{name: rider, namespace: boss-node-maintenance}")
        ),
        1,
    );
    let (rc, said) = lint_fixture(
        "comment-separator",
        HOSTPATH_LINT,
        &[("boss-node-maintenance.yaml", &smuggled)],
    );
    assert_eq!(
        rc, 1,
        "a document behind `--- # comment` is a document:\n{said}"
    );
    assert!(
        said.contains("hostPath: {path: /}"),
        "and its flow-style mount is the line named:\n{said}"
    );

    // A whole document ON the separator line.
    let inline = real.replacen(
        before_policy,
        &format!(
            "--- {{apiVersion: v1, kind: Pod, metadata: {{name: rider, namespace: boss-node-maintenance}}, \
             spec: {{containers: [{{name: c, image: x}}], volumes: [{{name: h, hostPath: {{path: /}}}}]}}}}\n\
             {before_policy}"
        ),
        1,
    );
    let (rc, said) = lint_fixture(
        "document-on-the-separator",
        HOSTPATH_LINT,
        &[("boss-node-maintenance.yaml", &inline)],
    );
    assert_eq!(
        rc, 1,
        "content after the dashes opens the document:\n{said}"
    );

    // Another file, flow style, the namespace quoted (mutant 20): the
    // mount is seen, and a namespace the reader cannot take as a plain
    // block value is not matched to a privileged one.
    let (rc, said) = lint_fixture(
        "flow-style-elsewhere",
        HOSTPATH_LINT,
        &[(
            "rider.yaml",
            &flow_rider("\n  name: rider\n  namespace: \"boss-node-maintenance\""),
        )],
    );
    assert_eq!(rc, 1, "{said}");
    assert!(said.contains("rider.yaml"), "it names the file:\n{said}");
}

// ---------------------------------------------------------------------
// Review 9b05cc55: what a YAML reader decodes and a line reader does not.
// ---------------------------------------------------------------------

/// Every `*.yaml` / `*.yml` under `infra/`, as (path, raw bytes).
fn infra_yaml_bytes() -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, root, out);
            } else if p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("yaml") || x.eq_ignore_ascii_case("yml"))
            {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                let bytes =
                    std::fs::read(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
                out.push((rel, bytes));
            }
        }
    }
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("infra"), &root, &mut out);
    assert!(
        out.len() >= 25,
        "only {} yaml file(s) under infra/ — the walk lost its subject",
        out.len()
    );
    out
}

/// What makes a file's LINES, as this pin and every shell lint count
/// them, differ from the lines a YAML reader sees: a line break that is
/// not LF (a carriage return, U+0085, U+2028, U+2029 — each ends a line
/// for YAML and for nothing else here), or bytes that are not plain
/// UTF-8 (a YAML reader decodes UTF-16 by its byte-order mark; a line
/// reader sees noise). `None` when the bytes hide nothing of the kind.
fn hides_a_line(bytes: &[u8]) -> Option<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Some("is not valid UTF-8".to_string());
    };
    let named = |c: char| match c {
        '\r' => Some("a carriage return"),
        '\u{0085}' => Some("U+0085 NEXT LINE"),
        '\u{2028}' => Some("U+2028 LINE SEPARATOR"),
        '\u{2029}' => Some("U+2029 PARAGRAPH SEPARATOR"),
        '\u{feff}' => Some("a byte-order mark"),
        '\0' => Some("a NUL"),
        _ => None,
    };
    text.split('\n').enumerate().find_map(|(n, l)| {
        l.chars()
            .find_map(named)
            .map(|what| format!("line {} carries {what}", n + 1))
    })
}

/// B2a. A line that is one comment to this pin was a comment plus a
/// second, privileged container to kubectl (mutants 39 and 42): the
/// lines after a lone CR or a U+2028. Refused anywhere, comments
/// included, in every yaml under `infra/` — the trim manifest too.
#[test]
fn no_yaml_under_infra_hides_a_line_from_a_line_reader() {
    let found: Vec<String> = infra_yaml_bytes()
        .iter()
        .filter_map(|(file, bytes)| hides_a_line(bytes).map(|what| format!("{file}: {what}")))
        .collect();
    assert!(
        found.is_empty(),
        "a yaml under infra/ carries a line break only a YAML reader honours, or is not \
         plain UTF-8. Every check on these files — this pin, the hostPath lint, a reviewer's \
         diff — reads lines ending in LF; kubectl also ends a line at a carriage return, \
         U+0085, U+2028 and U+2029, so what follows one inside a `# comment` is a comment \
         here and YAML there (review 9b05cc55, B2a: a second privileged container in the \
         trim CronJob passed this pin that way). Remove the character:\n  {}",
        found.join("\n  ")
    );
    // The reader itself, on the reviewer's two inputs and on plain text.
    for (sep, name) in [("\r", "carriage return"), ("\u{2028}", "U+2028")] {
        let line = format!("  # a note{sep}  - name: rider{sep}    image: busybox\n");
        assert!(
            hides_a_line(line.as_bytes()).is_some_and(|w| w.contains(name)),
            "{name} is found"
        );
    }
    assert_eq!(hides_a_line(b"a: b\n# plain \xe2\x80\x94 dash\n"), None);
    assert!(hides_a_line(b"\xff\xfea\x00:\x00").is_some());
}

/// The column of the node a line opens — past its indent and any `- `
/// entries — and the column of the last such dash.
fn node_col(l: &str) -> (usize, Option<usize>) {
    let b = l.as_bytes();
    let mut i = b.iter().take_while(|c| **c == b' ').count();
    let mut dash = None;
    while b.get(i) == Some(&b'-') && b.get(i + 1) == Some(&b' ') {
        dash = Some(i);
        i += 2;
        while b.get(i) == Some(&b' ') {
            i += 1;
        }
    }
    (i, dash)
}

/// Whether `s` ends in a block-scalar indicator (`|` or `>`, then at
/// most two of `+`, `-` or a digit), and where that indicator starts.
fn indicator_at(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let tail = b
        .iter()
        .rev()
        .take_while(|c| matches!(**c, b'+' | b'-' | b'0'..=b'9'))
        .count()
        .min(2);
    let at = b.len().checked_sub(tail + 1)?;
    matches!(b[at], b'|' | b'>').then_some(at)
}

/// The parent column of a block scalar a line opens, when the line is
/// PLAINLY a header: `key: |` or `- |`, nothing after the indicator, and
/// not one quote or flow bracket anywhere on it. Anything less plain is
/// not taken as a header (`x: {b: |` is a flow mapping whose value is
/// the one character), so the lines after it are read, not skipped.
fn block_header(s: &str) -> Option<usize> {
    if s.contains(['{', '[', '}', ']', '"', '\'']) {
        return None;
    }
    let at = indicator_at(s)?;
    let (col, dash) = node_col(s);
    if at == col {
        return dash; // `- |`: the entry's dash is the parent
    }
    let head = &s[col..at];
    (head.ends_with(' ') && head.trim_end().ends_with(':')).then_some(col)
}

/// One line's flow brackets, counted outside its quoted scalars and its
/// trailing comment: `Ok(opened - closed)`, or what could not be read.
fn flow_balance(s: &str) -> Result<i64, &'static str> {
    let b = s.as_bytes();
    let (mut i, mut net, mut prev) = (0usize, 0i64, b' ');
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            if j >= b.len() {
                return Err("a double-quoted scalar that does not close on its line");
            }
            (i, prev) = (j + 1, b'"');
            continue;
        }
        if c == b'\'' && matches!(prev, b' ' | b'[' | b'{' | b',' | b':') {
            let mut j = i + 1;
            loop {
                match (b.get(j), b.get(j + 1)) {
                    (Some(b'\''), Some(b'\'')) => j += 2,
                    (Some(b'\''), _) => break,
                    (Some(_), _) => j += 1,
                    (None, _) => {
                        return Err("a single-quoted scalar that does not close on its line");
                    }
                }
            }
            (i, prev) = (j + 1, b'\'');
            continue;
        }
        if c == b'#' && matches!(prev, b' ' | b'\t') {
            break;
        }
        net += match c {
            b'{' | b'[' => 1,
            b'}' | b']' => -1,
            _ => 0,
        };
        (i, prev) = (i + 1, c);
    }
    Ok(net)
}

/// B3a. The spellings that let a scalar's TEXT differ from the bytes on
/// its line, each with its 1-based line — outside comment lines and
/// outside the body of a plain block scalar, where shell continuations
/// legitimately end lines in a backslash.
///
/// This is a NET, not a YAML reader, and it is cut to fail closed: what
/// it cannot place it reports. It refuses
///   * a tag (`!!binary`, a verbatim `!<…>`, a named handle `!x!`) and
///     a `%` directive — kubectl decodes a `!!binary` scalar to a
///     string no line contains;
///   * a line ending in a backslash, an odd count of double quotes, and
///     a quoted scalar that does not close on its line — a double-quoted
///     scalar continued by `\` joins its pieces with nothing between;
///   * an explicit key (`? `), block or flow — the way a KEY is split so;
///   * flow brackets that do not balance on the line — so a flow
///     collection never spans lines, and a `b: |` on a later line can
///     only be the block-scalar header it looks like;
///   * a tab; and a line that ends in `|` or `>` without being a plain
///     header.
fn hidden_spellings(text: &str) -> Vec<(usize, &'static str)> {
    let indent = |l: &str| l.bytes().take_while(|c| *c == b' ').count();
    let mut found = Vec::new();
    let mut parent: Option<usize> = None; // a header was the last line read
    let mut body: Option<usize> = None; // inside a block scalar: its indent
    for (n, l) in text.split('\n').enumerate() {
        let blank = l.trim().is_empty();
        if let Some(col) = parent {
            if blank {
                continue;
            }
            parent = None;
            if indent(l) > col {
                body = Some(indent(l));
                continue;
            }
        }
        if let Some(min) = body {
            if blank || indent(l) >= min {
                continue;
            }
            body = None;
        }
        if blank || l.trim_start().starts_with('#') {
            continue;
        }
        let s = l.trim_end();
        let mut hit = |what| found.push((n + 1, what));
        let b = s.as_bytes();
        let handle = (0..b.len()).any(|i| {
            b[i] == b'!'
                && b[i + 1..]
                    .iter()
                    .position(|c| !(c.is_ascii_alphanumeric() || *c == b'-'))
                    .is_some_and(|k| k > 0 && b[i + 1 + k] == b'!')
        });
        if s.contains("!!") || s.contains("!<") || handle {
            hit("a tag");
        }
        if s.starts_with('%') {
            hit("a directive");
        }
        if s.ends_with('\\') {
            hit("a line ending in a backslash");
        }
        if s.matches('"').count() % 2 == 1 {
            hit("an odd count of double quotes");
        }
        if l.contains('\t') {
            hit("a tab");
        }
        let (col, _) = node_col(s);
        let rest = &s[col..];
        let flow_key = (0..b.len()).any(|i| {
            b[i] == b'?'
                && matches!(b.get(i + 1), None | Some(b' '))
                && s[..i].trim_end().ends_with(['{', '[', ','])
        });
        if rest == "?" || rest.starts_with("? ") || flow_key {
            hit("an explicit key");
        }
        match flow_balance(s) {
            Err(what) => hit(what),
            Ok(0) => {}
            Ok(_) => hit("flow brackets that do not balance on the line"),
        }
        match block_header(s) {
            Some(col) => parent = Some(col),
            None if indicator_at(s).is_some_and(|at| at == 0 || b[at - 1] == b' ') => {
                hit("a line ending in `|` or `>` that is not a plain block-scalar header");
            }
            None => {}
        }
    }
    found
}

#[test]
fn the_line_reader_refuses_what_it_cannot_show() {
    let kinds = |text: &str| -> Vec<&'static str> {
        hidden_spellings(text).into_iter().map(|(_, w)| w).collect()
    };
    // The reviewer's spellings (run 9b05cc55, mutants 35, 36, 37).
    let split_value = "metadata:\n  namespace: \"boss-node-\\\n    maintenance\"\n";
    assert!(
        kinds(split_value).contains(&"a line ending in a backslash")
            && kinds(split_value).contains(&"an odd count of double quotes"),
        "a value continued by backslash-newline: {:?}",
        kinds(split_value)
    );
    let split_key = "securityContext:\n  ? \"privi\\\n    leged\"\n  : true\n";
    assert!(
        kinds(split_key).contains(&"an explicit key"),
        "a key split through an explicit key: {:?}",
        kinds(split_key)
    );
    assert_eq!(
        kinds("  namespace: !!binary Ym9zcy1ub2RlLW1haW50ZW5hbmNl\n"),
        ["a tag"]
    );
    assert_eq!(
        kinds("  namespace: !<tag:yaml.org,2002:binary> Ym9zcw==\n"),
        ["a tag"]
    );
    assert_eq!(
        kinds("%TAG !e! tag:yaml.org,2002:\n"),
        ["a tag", "a directive"]
    );
    assert_eq!(kinds("spec: {a: 1, ? b : 2}\n"), ["an explicit key"]);

    // What the tree legitimately holds passes: a shell continuation in a
    // block scalar under `key: |`, under `- |`, and under `- key: |-`;
    // flow collections that close on their line; quotes that pair.
    let plain = "a:\n  run: |\n    curl -sf \\\n      \"$URL\" | jq '.x[] | {a}'\n  args:\n    - |\n      set -u\n      f() {\n        echo \"it's\" \\\n          done\n      }\n  list:\n    - name: |-\n        x \\\n    - {name: c, env: [\"a\", 'b']}\n  s: 'it''s' # {\n";
    assert_eq!(kinds(plain), Vec::<&str>::new(), "legitimate shapes pass");

    // THE EXEMPTION'S OWN HOLES, each closed.
    // A flow mapping's `b: |` is not a header: the next line is read.
    let flow_bar = "x: {b: |\n  , \"privi\\\n  leged\": true}\n";
    assert!(
        kinds(flow_bar).contains(&"flow brackets that do not balance on the line")
            && kinds(flow_bar).contains(&"a line ending in a backslash"),
        "{:?}",
        kinds(flow_bar)
    );
    // A flow collection may not stay open across lines at all, so a
    // later `b: |` cannot be inside one.
    assert!(
        kinds("spec: {\n  b: |\n    , \"a\\\n    b\": 1 }\n")
            .contains(&"flow brackets that do not balance on the line")
    );
    // …not even with the closing bracket counted from inside a string.
    assert!(kinds("spec: {a: \"}\"\n").contains(&"flow brackets that do not balance on the line"));
    // A sibling key of `- a: |` sits at the dash's column plus two: it
    // is not body, and is read.
    let sibling = "- a: |\n    text\n  \"b\\\n   c\": 1\n";
    assert!(
        kinds(sibling).contains(&"a line ending in a backslash"),
        "{:?}",
        kinds(sibling)
    );
    // A block scalar ends where its indent does.
    let after = "a: |\n  body \\\nb: \"x\\\n  y\"\n";
    assert!(
        kinds(after).contains(&"a line ending in a backslash"),
        "{:?}",
        kinds(after)
    );
    // A header with anything quoted or bracketed on it is not one.
    assert!(
        kinds("\"a\": |\n  x \\\n").contains(&"a line ending in a backslash"),
        "a quoted key's block scalar is read rather than skipped"
    );
    // A quoted scalar left open, of either kind.
    assert!(
        kinds("a: 'open\n").contains(&"a single-quoted scalar that does not close on its line")
    );
    assert!(kinds("a: \"open\n").iter().any(|k| k.contains("double")));
}

/// B3a, on the tree. Every OTHER yaml under `infra/`: the trim manifest
/// is held whole by `the_namespace_holds_exactly_these_objects`.
#[test]
fn no_other_yaml_spells_a_scalar_its_lines_do_not_show() {
    let mut findings = Vec::new();
    for (file, bytes) in infra_yaml_bytes() {
        if file == MANIFEST {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (line, what) in hidden_spellings(&text) {
            let shown = text.split('\n').nth(line - 1).unwrap_or("").trim();
            findings.push(format!("{file}:{line}: {what}: {shown}"));
        }
    }
    assert!(
        findings.is_empty(),
        "a yaml under infra/ spells a scalar in a way its lines do not show. \
         `nothing_else_in_the_tree_asks_for_what_the_namespace_admits` looks for the \
         namespace's name, `hostPath`, `privileged` and the rest as text on a line; a \
         double-quoted scalar continued by a backslash, an explicit key, a `!!binary` \
         scalar or a flow collection left open across lines lets kubectl read one of those \
         words where no line carries it (review 9b05cc55, B3a: a privileged, host-mounting \
         DaemonSet in the namespace passed every check that way). Write it plainly — one \
         scalar, one line — or, if it is shell, inside a `key: |` block scalar:\n  {}",
        findings.join("\n  ")
    );
}

/// The lint a builder runs locally refuses the same line breaks, by
/// running it on the reviewer's mutant 39.
#[test]
fn the_hostpath_lint_refuses_a_line_break_it_cannot_read() {
    let real = read(MANIFEST);
    let before = "          volumes:\n            - name: gate";
    assert!(real.contains(before), "the fixture's insertion point");
    for (sep, case) in [("\r", "carriage-return"), ("\u{2028}", "line-separator")] {
        let hidden = format!(
            "            # a note{sep}            - name: rider{sep}              image: busybox{sep}              securityContext: {{privileged: true}}\n"
        );
        let (rc, said) = lint_fixture(
            case,
            HOSTPATH_LINT,
            &[(
                "boss-node-maintenance.yaml",
                &real.replacen(before, &format!("{hidden}{before}"), 1),
            )],
        );
        assert_eq!(rc, 1, "{case}:\n{said}");
        assert!(
            said.contains("boss-node-maintenance.yaml") && said.contains("line break"),
            "{case}: it names the file and why:\n{said}"
        );
    }
}

/// N8 of review 9b05cc55: the manifest was held for review only because
/// three lints happen to name its file. Its header now makes the hold's
/// own claim (`CLAIM` in boss-cli's mutating_verb.rs), so it is held BY
/// NAME; this fails if the sentence is reworded out of the header.
#[test]
fn the_manifest_claims_its_own_review() {
    let prose = read(MANIFEST)
        .lines()
        .map(|l| l.trim_start().trim_start_matches('#'))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        prose.contains(
            "every change to this file is a credentials-area car and waits for an adversarial review"
        ),
        "{MANIFEST} no longer makes the sentence the park hold reads (boss-cli \
         mutating_verb.rs, CLAIM). Without it the file is held only while some covered \
         script happens to name it"
    );
}
