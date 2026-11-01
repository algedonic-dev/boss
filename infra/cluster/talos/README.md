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
  every gate, and nothing would have said why. (The gate left that
  seed for the gate volume below on 2026-09-30, and the PV was retired
  on 2026-10-10, `1f13b894`; the directory stays as the daily trim
  pod's handle on EPHEMERAL.)
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
- **w-2, since 2026-10-09** — the same image-GC pair, 50/40, and nothing
  else (backlog `666e0570`). Declared to be APPLIED, not as found: w-2
  had no declaration, and its disk filled with old boss-ci images to
  Longhorn's 25% floor while the kubelet's default 85% line had never
  fired. The forge mirror is live there and left undeclared on purpose
  (it reads `UNDECLARED`): a patch carrying a live entry is refused
  unless it is idempotent, and whether Talos appends mirror endpoints is
  unmeasured. The file's own header has the measurement.
- **w-1, since 2026-10-19** — the same image-GC pair, 50/40, for the
  same reason at nine times the size (backlog `67e98635`): 231.2 GiB
  available of 928.9 against Longhorn's 25% floor of 232.2, imagefs
  535.2 GiB, so every dev-build worker volume provisioned and never got
  a replica. Declared to be applied; its other entries are live, so it
  reaches the node as a NARROWED patch (below).
- **every node but w-2** — `machine.registries.mirrors` for
  `10.20.0.15:3000`, the forge's registry every image in the cluster is
  pulled through.

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

## The image a blank node network-boots: `netboot.toml`

`netboot.toml` declares ONE Talos schematic and version, with the
sha256 of its netboot kernel and initramfs and the Image Factory's metal
kernel args (backlog `ee31ab73`, 2026-10-19). The forge serves it ONLY to
the NICs listed in its `netboot_macs` — every other PXE client gets no
answer, and an empty list answers nobody, so an installed node that tries
the network first still boots its disk (review `bcff47dd` N1, backlog
`8cf6faf7`) — proxy-DHCP (no leases, no DNS), TFTP for iPXE,
HTTP for the script and the pair — through `infra/forge/netboot.sh`,
which the forge converge runs, and serves nothing that does not hash to
this file. The node lands in maintenance mode and joins nothing; the
config is applied by hand as before. Changing the version is an edit to
this file, measured as its header says. The schematic is spelled there
and nowhere else in the tree (`netboot_sh.rs` holds it to that).

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
`node-converge` builds that fragment itself (`check-declared.sh --absent`,
below). The check afterwards is what proves the fragment equalled the
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
  patch appends (the doubling above). If ANY entry it declares, in any
  class, is already live, the whole declaration is neither dry-run nor
  applied: the plan NARROWS it FIRST to the entries that read ABSENT
  live (`check-declared.sh --absent <node>`, which re-emits them from the
  DECLARED values in canonical form; backlog `9cf1673d`, built for
  `67e98635`), prints that patch beside the declaration, judges it as a
  declaration of its own (read classes, its own two dry runs) and
  applies exactly it. First, because the copy Talos's merge would append
  can be a change it refuses outright in no-reboot mode: the first live
  plan dry-ran w-1's whole declaration, Talos answered "this config
  change can't be applied in immediate mode" over a second seed files
  entry, and the narrowed patch was never built (backlog `a3b1084f`). It
  refuses instead, before any dry run, when a declared entry DRIFTs
  live — adding entries cannot correct a drift, and narrowing past one
  would read back as converged with the drift still there — or when no
  entry is absent, since the patch could then only add a second copy.
  Only a node with every entry ABSENT has its whole declaration dry-run;
- Talos's diff is rendered with each context line reduced to its key,
  a changed line shaped like a credential refuses the plan unprinted,
  and the check's findings appear as a verdict and a key each, never a
  live value;
- the apply (`--mode=no-reboot`), then the live config read through
  `check-declared.sh` until every declared entry is `MATCH` — for a
  narrowed patch, every entry IT carries `MATCH` and every declared
  entry it did not carry reading exactly as it did before the apply, so
  a DOUBLED entry left as found is no failure and a patch that disturbed
  an entry it never named is no effect — and the
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
  it appears in the form it provably parsed. Every declaration here is
  canonical, and a test holds each node `infra/estate/estate.toml`
  names with a Talos role to having one. In the live config, which Talos emits, content on a
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

**When the append rule bites, the live entries are not the problem.**
Applying a whole declaration file is not idempotent for a list Talos's
merge cannot key, so a declaration whose entries are already live cannot
be re-applied — w-1 (its seed file entry DOUBLED as last read, its
mount, mirror and gate volume live), and every control plane if Talos
appends mirror endpoints. **Never remove a working live entry to unblock
the verb:** the seed mount, the forge mirror and the image-GC thresholds
are what the node runs on. The verb narrows instead: since 2026-10-19
(backlog `9cf1673d`'s first half, built for `67e98635`, when w-1's
image-GC pair could not reach the node and its Longhorn disk sat under
the scheduling floor) a plan for such a node applies the entries that
read ABSENT and nothing else. A DOUBLED entry is left as found; a verb
that removes one copy, signed by passkey, is the half of `9cf1673d`
still open.

A plan that exits 1 saying Talos's dry run "did not read as a summary
and a config diff" is about the output format, not the node: the
parser reads talosctl's `Dry run summary:` / `Config diff:` shape,
optionally opened with the node's address or name, and fails closed on
anything else. The first live render is its measurement.

## A user volume becomes Longhorn capacity: `add-longhorn-disk`

A `UserVolumeConfig` applied by `node-converge` is a mounted
filesystem at `/var/mnt/<name>` and nothing more: Longhorn knows a disk
only when its node's `nodes.longhorn.io` object lists it under
`spec.disks`. Until backlog `493a02b6` (2026-10-19) no door wrote that
list, so a node maintenance window that fitted a disk ended at a
mounted volume nothing used. The second door is the ops pair
`plan-a-longhorn-disk <node> <volume>` and `add-longhorn-disk <node>
<volume> <plan_sha256>` (`infra/forge/longhorn-disk.sh`), in the same
plan-and-passkey shape. It reads the mount from the declaration here,
never from the packet, and refuses:

- a volume `patches/<node>.yaml` does not name as a `UserVolumeConfig`,
  or a declaration `check-declared.sh --only-read-classes` refuses (the
  `!system_disk` allowlist above);
- a volume Talos does not read `ready` at `/var/mnt/<name>`, or one on
  the disk `EPHEMERAL` lives on;
- a path any PersistentVolume or pod on the node already mounts — which
  is how w-1's gate volume (`/var/mnt/gate`, the local PV `gate-w-1` and
  the trim pod's hostPath) is refused;
- a path that is, is under, or holds an existing Longhorn disk.

The write is one compare-and-set patch adding the disk `u-<name>`,
untagged and schedulable, and its read-back is Longhorn's own: the disk
`Ready` and `Schedulable` with a `diskUUID` of its own, every other disk
unchanged. The order for a worker's second disk is: declare its
`UserVolumeConfig` in `patches/<node>.yaml` and land it, fit the disk,
`node-converge <node>`, then `plan-a-longhorn-disk <node> <volume>` and
`add-longhorn-disk` under the passkey, then `node-disk-usage <node>`
lists the new disk. No declaration here names such a volume yet: the
door exists before the disk it is for. A node whose ONLY disk is
replaced needs none of this door — but it may need the next pair down:
Longhorn can keep the OLD disk's record for the reinstalled system disk
(join-a-node `829f75a8`, 2026-10-19: a one-bay worker reinstalled on its
larger disk, and on 2026-10-21 w-2's default disk still read
`DiskFilesystemChanged` under the old `diskUUID`, placing nothing).

The inverse is the pair `plan-a-longhorn-disk-removal <node> <volume>`
and `remove-longhorn-disk <node> <volume> <plan_sha256>`
(`infra/forge/remove-longhorn-disk.sh`, backlog `b791c5b4`): it takes
`u-<name>` at `/var/mnt/<name>` back out of `spec.disks` — the disk the
add door places and no other, so a default disk cannot be named — and
it is the way out for a disk Longhorn never read `Ready` after the add.
It refuses while the disk holds anything: a replica on its `diskUUID`
or on its node and path, a scheduled replica or backing image in
Longhorn's `diskStatus`, a nonzero `storageScheduled`, or a backing
image file. It does not evict; move a replica off first with
`move-volume-replica` or `retire-volume-replica`. The write makes the
disk unschedulable, reads what is on it again, and only then removes
it, each a compare-and-set; if a replica landed in between, the disk
is left unschedulable and nothing is removed. Its read-back is the disk
gone from `spec.disks` and from Longhorn's `diskStatus`, every other
disk unchanged. The Talos volume stays mounted; un-declaring it is a
separate change to `patches/<node>.yaml` and a `node-converge`.

The third pair repairs a node's DEFAULT disk whose record drifted from
the disk under it: `plan-a-longhorn-disk-reregistration <node>` and
`reregister-longhorn-disk <node> <plan_sha256>`
(`infra/forge/reregister-longhorn-disk.sh`, backlog `36d2c484`). The
packet names only the node; the disk is the one at `/var/lib/longhorn`
named `default-disk-<hex>`. It refuses unless Longhorn reads that disk
`Ready=False` with reason `DiskFilesystemChanged` and the disk's own
`longhorn-disk.cfg`, read from the node through `talosctl read`, names a
`diskUUID` other than the record's — and, as the removal does, while
anything names the record on either UUID. The write makes the record
unschedulable and reads its holdings again, removes it, waits for
Longhorn to drop the stale status, and adds the same record back
schedulable, each a compare-and-set; Longhorn then records the UUID the
file names. Its read-back is the disk `Ready` and `Schedulable` under
that UUID with a `storageMaximum` above zero, every other disk
unchanged.
