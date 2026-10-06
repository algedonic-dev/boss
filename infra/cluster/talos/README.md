# infra/cluster/talos — the machine-config entries the pipeline depends on

`patches/<node>.yaml` is the **declaration** of every non-secret Talos
machine-config entry a BOSS node carries that the pipeline relies on.
`check-declared.sh` reads a node's LIVE config against it. Nothing here
holds a credential, and nothing here runs `talosctl` — the forge's
`node-converge` verb applies a declaration (below).

## Why this directory exists

CLAUDE.md's rule: *an imperative cluster change has an expiry*, and
substrate the pipeline relies on is declared where the pipeline can
read it. Until 2026-09-15 (backlog `08430090`) the entries below lived
only in `~/talos-homelab/v2` on a laptop, applied by hand with
`talosctl patch machineconfig`:

- **w-1** — `machine.files` creating `/var/local/gate-seed` and
  `machine.kubelet.extraMounts` binding it into the kubelet's mount
  namespace. The gate's warm seed is a `local` PersistentVolume on that
  directory (`infra/cluster/manifests/gate-seed-local.yaml`); without
  the bind a pod mounting it sat nine hours at ContainerCreating
  (`d42d4967`). A rebuilt or re-imaged w-1 without these entries hangs
  every gate, and nothing would have said why.
- **w-1, since 2026-09-30** — a second document, a `UserVolumeConfig`
  named `gate`: w-1's second NVMe as one xfs volume Talos mounts at
  `/var/mnt/gate`, chosen by the selector `!system_disk &&
  disk.transport == "nvme" && disk.size > 500u * GB` and never by a
  serial or device name. ALL the gate's disk I/O moves there — seed and
  workspaces on one filesystem, because a reflink cannot cross two
  (backlog `52ea56ac`, design `8457c07b`). Declared before it was
  applied: the check reads it `ABSENT` until David applies it. There is
  deliberately NO `extraMounts` entry for `/var/mnt/gate`: the kubelet
  MkdirAll's every extraMounts source when it starts, which would put
  the path on EPHEMERAL whenever the volume is absent and send the gate
  to the install disk silently (review `a79746c6`). Talos already binds
  `/var/mnt` into the kubelet as `rbind, rslave, ro` — read-only at the
  top, so a proof that fails with EROFS or a chown error points there
  (review `635317c5`); the proof pod decides whether that is enough. A
  missing disk is made loud by the gate runner, not by this
  declaration: `infra/gate-runner/run.sh` refuses to run off the gate
  disk.
- **cp-1, cp-2, cp-3** — `machine.kubelet.extraConfig` image-GC
  thresholds, one of the levers behind the 2026-09-11 outage of the
  system of record (design `16115a17`).
- **every node** — `machine.registries.mirrors` for `10.20.0.15:3000`,
  the forge's registry every image in the cluster is pulled through.

The values are each node's live config as David read it on 2026-09-15
(`talosctl -n <ip> get machineconfig -o yaml`), declared **as found,
per node**. Two things are worth knowing about what was found:

- w-1's gate-seed `files` entry is present **twice** live — two
  identical hand patches on 2026-09-12. It is declared once; the check
  reports the double as `DOUBLED` until it is removed live.
- The image-GC pair is **40/30 on cp-2 and 50/40 on cp-1 and cp-3**.
  Which pair is intended is an open question on `08430090`; the
  declaration records each node as it is, so the check reads `MATCH`
  today and the decision, when made, is one edit per node here.

PKI, tokens, the cluster secret, and anything else `talosctl gen
secrets` produces **never live here**. A patch is a fragment; the
secrets stay under `/etc/boss-ops` on the cluster-operator host
(design `1bc4b4ed`) and in the operator's own config until then.

## Reading a node against its declaration

The check takes the live config as **input** — a file, or stdin — so it
needs no talosconfig and runs anywhere python3 does (the pod, the gate
image, a Mac; no PyYAML needed):

```
talosctl -n 10.20.0.14 get machineconfig -o yaml | infra/cluster/talos/check-declared.sh w-1
infra/cluster/talos/check-declared.sh cp-2 cp-2.live.yaml
```

Node addresses come from the estate registry (`boss-api GET
/api/estate/nodes`, field `address`), not from a table here — the
registry is the one place that answers hardware questions. The check
itself needs no address: the live document names its own hostname, and
the check **refuses** to compare a document whose hostname is not the
node asked for (a wrong target answers instead of erroring).

One line per declared entry, in the vocabulary design `16115a17`
decided, then a summary:

| verdict | meaning | exit |
|---|---|---|
| `MATCH` | present live with the declared value | 0 |
| `DRIFT` | present live with a different value — both printed | 1 |
| `ABSENT` | declared, not live | 1 |
| `DOUBLED` | a declared `files`/`extraMounts` entry appears more than once live | 1 |
| `UNDECLARED` | live, in a class the check reads, named by no declaration | 0 (reported) |

Exit 2 is a usage refusal (no such node, unreadable input, wrong
hostname); 78 is no python3. The classes read for `UNDECLARED` are
`machine.files` under `/var/local`, every `extraMounts` entry, every
`imageGC*` key of `extraConfig`, every registry mirror, and every
`UserVolumeConfig` — so the
first run on a control plane will most likely list a mount the grep of
2026-09-15 showed but did not name. That is the check working: declare
it here, or leave it listed.

A declaration is one `machine:` fragment plus any number of
`UserVolumeConfig` documents after `---`, compared by `name`; any other
document kind is refused (exit 2) rather than skipped. A user volume
lives live as a second document of the same config, so feed the check
the config the node STORES, every document included:

```
talosctl -n 10.20.0.14 read /system/state/config.yaml | infra/cluster/talos/check-declared.sh w-1
```

(`get machineconfig -o yaml` is read too: a multi-document config
prints there as a block-string `spec:`.)

The check is a pure comparator. On the forge, `node-converge` feeds it
through the talosconfig in `/etc/boss-ops` — its plan reads the node
before, its write reads the node after; the comparison does not
change. (`talos-get` never reads `machineconfig`: its bytes carry the
cluster's keys.)

## Applying a declaration

A patch is a Talos strategic-merge fragment, exactly the shape
`talosctl patch machineconfig --patch @file` accepts:

```
talosctl -n <address> patch machineconfig --mode=no-reboot --dry-run --patch @<fragment>.yaml
talosctl -n <address> patch machineconfig --mode=no-reboot --patch @<fragment>.yaml
```

The kubelet restarts; running pods survive; **no reboot** (`files`,
`extraMounts`, `extraConfig`, `registries` and a `UserVolumeConfig` all
apply live). `--mode=no-reboot` makes that a constraint rather than an
expectation: the default `auto` REBOOTS the node if Talos judges the
change needs it, and a surprise reboot of w-1 takes the dev pod, the
gates in flight and its Longhorn replicas with it (review `a79746c6`);
`no-reboot` refuses instead. Read the dry-run's diff first: it must be
additions only. Then
read the node back through the check — verify is always the next
comparison, never the applier's exit code.

Two things the merge will not do for you:

- It **appends** list entries it cannot key — that is how w-1's seed
  file entry came to be doubled. Re-applying `w-1.yaml` does not repair
  a `DOUBLED` finding; remove the duplicate with `talosctl -n <address>
  edit machineconfig` and run the check again.
- It does not remove an entry you delete from a declaration. A removed
  entry reads as `UNDECLARED` live until it is removed live too.

So a node that already carries part of its declaration is patched with
a fragment of ONLY what it lacks, never the whole file again — the whole
of `w-1.yaml` re-applied would add a third copy of the seed file entry.
The check afterwards is what proves the fragment equalled the
declaration. For w-1's gate disk the fragment is the `UserVolumeConfig`
document ALONE. A `UserVolumeConfig` (Talos v1.10 and later) is
provisioned by the node's volume controller when the config lands —
read `talosctl get volumestatus u-<name> -o yaml` for its phase and
the device it landed on rather than assuming either; a document
fragment is accepted by `talosctl patch machineconfig`, which matches
documents by kind and name and appends a new one.

## The door: `node-converge`

The bounded act design `1bc4b4ed` decided is built (backlog
`9d56c616`): the ops verbs `plan-a-node-converge <node> <tree_sha>` and
`node-converge <node> <tree_sha> <plan_sha256>`
(`infra/forge/node-converge.sh`), on the forge, through the talosconfig
in `/etc/boss-ops`. File the plan as an ops-request; David's passkey
signs the rendered plan; the runner then runs the write, which re-renders
and applies nothing unless the plan still hashes to what was signed. What
it does and refuses, in order:

- the node is one the estate registry declares with a Talos role, and
  the cluster agrees on its address and whether it is a control plane;
- the patch is `patches/<node>.yaml` read out of git at `tree_sha`, a
  full commit on the history the forge's checkout converged to, and the
  very blob its HEAD carries — never a path or patch text from a packet;
- every entry it declares is in a class `check-declared.sh` reads
  (`check-declared.sh --only-read-classes <node>`), because the effect
  is read back through it. A `UserVolumeConfig` document is read back by
  name, so it may be applied, but only when its `diskSelector` match is
  in an ALLOWLIST grammar: `term (&& term)*`, each term exactly
  `!system_disk` or `disk.<field> <op> <literal>` (`<op>` one of `==`
  `!=` `<` `<=` `>` `>=`; a literal a number with an optional `u` and
  `* GB|MB|TB`, or a double-quoted string without a quote, backslash or
  `&`), exactly one `!system_disk`, and no `(`, `)`, `[`, `]`, `//`,
  `/*` or newline anywhere. A volume Talos provisioned on the install
  disk would take EPHEMERAL, and w-1's Longhorn replicas, with it. Any
  other document
  kind is refused. Under `extraConfig`, only the kubelet keys the tree
  declares today are admitted (the image-GC pair, `EXTRA_CONFIG_KEYS` in
  the check), because `extraConfig` passes any kubelet
  setting straight through, anonymous auth and authorization mode
  among them;
- `talosctl patch machineconfig --mode=no-reboot --dry-run`, whose
  summary must read "Applied configuration without a reboot" and whose
  diff must change something — so an applied plan is never run twice;
- the same dry run with the patch applied TWICE: when it differs, the
  patch appends (the doubling above), and it is refused if ANY entry it
  declares, in any class, is already live;
- Talos's diff is rendered with each context line reduced to its key,
  a changed line shaped like a credential refuses the plan unprinted,
  and the check's findings appear as a verdict and a key each, never a
  live value;
- the apply (`--mode=no-reboot`), then the live config read through
  `check-declared.sh` until every declared entry is `MATCH`, and the
  Node read until it is `Ready=True` with its kubelet lease renewed
  after the apply, and read again at the end of the bound (every
  successful run takes the whole 120 s) — the verb's effect line, or
  exit 1 saying the node is NOT proven. A check that could not read the
  live config says `UNREADABLE` (exit 2), and its exit 0 or 1 counts
  only beside its own summary line.
- a declaration is in CANONICAL FORM: less its comments and blank lines,
  it must be byte-identical to the one block form `check-declared.sh`
  re-emits from what it parsed — two-space indent, `key: value`, `- `
  items with a mapping item's first key on the dash line, `|` for a
  multi-line string, `---` alone between documents, and a value plain
  when plain text can only be read as itself, single-quoted otherwise.
  No flow collections, tags, anchors, directives or `...`. Three times
  this reader and Talos's YAML read different structure from one text
  (reviews `be5ba8f7`, `5ed37182`, `a7fa61ec`); canonical form closes
  the class, since anything the reader might misread is refused unless
  it appears in the form it provably parsed. The four declarations here
  are canonical. In the live config, which Talos emits, content on a
  `---` line, a non-empty flow collection and lines left after a
  document's root are refused;
- the check reads a line where Talos does: a declaration or live config
  carrying NEL, LS or PS (which Talos's YAML breaks lines at and a
  `\n`-only reader would fold into a comment, hiding a whole document or
  a `cluster` key), CR, TAB, a byte-order mark past byte 0, or any other
  control character is refused before it is parsed, naming the line and
  the codepoint. Ordinary non-ASCII, such as the em dashes in these
  files' comments, is text. The check prints every value as JSON, so no
  live value can start a line of its own.

Reboot, upgrade, reset and etcd membership never become verbs.

**When the append rule refuses, the live entries are not the problem.**
Applying a whole declaration file is not idempotent for a list Talos's
merge cannot key, so a declaration whose entries are already live cannot
be re-applied — w-1 today (its seed file entry DOUBLED, its mount and
mirror live), and every control plane if Talos appends mirror
endpoints. **Never remove a working live entry to unblock the verb:**
the seed mount, the forge mirror and the image-GC thresholds are what
the node runs on, and de-duplicating a DOUBLED entry by hand only
leaves a MATCH, which is refused the same way. The remedy is a
declaration narrowed, for that run, to the entries that read ABSENT on
the node, or the partial patch that applies only those (a follow-up
item).

A plan that exits 1 saying Talos's dry run "did not read as a summary
and a config diff" is about the output format, not the node: the
parser reads talosctl's `Dry run summary:` / `Config diff:` shape,
optionally opened with the node's address or name, and fails closed on
anything else. The first live render is its measurement.
