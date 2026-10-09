# BOSS ops-request verb allowlist

THE authority for what a host runner will execute. In-tree, reviewed,
versioned: changing what a host runs for a packet is a PR, never a
packet.

## One file per verb

**This directory is the allowlist.** A verb is `<name>.json` here, and
the verb's name — the `metadata.verb` a packet carries — IS the file
name. Adding a verb is dropping a file in; it touches no shared line.

It used to be one file, `infra/ops/verbs.json`, one JSON object — and a
JSON object has no uncontended insertion point: appending contends on
the previous entry's trailing comma and the closing brace, inserting
alphabetically contends with a neighbour. On 2026-09-12 three cars each
added a verb, two inserted before the same key, and the conductor left
one behind (`conflict: infra/ops/verbs.json`) — the shape CLAUDE.md §9a
records for `rules.toml` before dispatcher rules became one file each
under `infra/dispatcher/rules/`. Backlog 5086842d collapsed this one the
same way.

Every reader DERIVES the allowlist from this directory; none holds a
list of verbs:

- `infra/ops/verbs-allowlist.sh` assembles `{"verbs": {<name>: <file>}}`
  from `*.json` here — the one derivation that `infra/ops/ops-runner.sh`
  (the runner on the forge and boss-gcp) and the two allowlist lints
  (`infra/lint/a-verb-declares-the-hosts-it-serves.sh`,
  `infra/lint/the-controls-are-bounded-verbs.sh`) all call. It refuses
  (exit 78, naming the fault) on a missing or empty directory, a file
  that is not one verb object, or a name the runner could not match.
- `boss ops` (`crates/orchestrators/boss-cli`) compiles the verbs in, so
  its `build.rs` lists this directory at build time and generates the
  `include_str!` table; a test pins that set to the directory.
- `infra/oss-quickstart/Dockerfile` COPYs the directory into the image's
  build stage, because boss-cli reads it while compiling there.

A `README.md` beside the verbs is prose, not a verb — only `*.json` is
read.

## Structured read results

A verb may declare `"capture": "separate-streams"` to retain stdout and
stderr separately on its execute receipt, under `streams`. Each stream
holds base64 bytes, the original byte count and SHA-256, the retained
byte count, and `complete`. Encoding conserves binary bytes and trailing
newlines. Each retained stream is bounded by `OPS_OUTPUT_CAP`; a larger
stream is explicitly incomplete. A capture failure records
`streams_unread` and no stream record. Ordinary verbs clear these keys
and keep their existing interleaved `output`.

For opted-in verbs, diagnostic `output` contains stdout followed by
stderr and retains its existing cap. Consumers read the verb's exit
first, then require complete streams and verify decoded bytes against
their counts and digest before parsing a proposal. Capturing bytes does
not judge JSON, approve a plan, or authorize a follow-on write. A timeout
can leave a completely captured partial answer with a failed exit.

The explicit volume-plan reader opts in so its plan bytes remain apart
from live-space diagnostics and its printed hash (backlog f3a09e07,
parent 53c8cb72).

## Discovered remedies

An approval verb may declare `discovery_remedies` with `finding_class`,
`scope`, `discovery_verb` and ordered `arg_fields`. The fields name direct
strings on that scope's finding; joined with `/` they must conserve its
identity. The discovery verb must be a same-host READ-ONLY registry row
with separate-stream capture and exactly those initial mutation params.
It proposes the remaining explicit arguments; it never receives the
signed hash or permission to mutate.

The estate reactor first applies the final remedy's whole-board,
recent-request and declined-episode guards, then files one native
discovery request for that final successor. On completion the second
reactor fetches the authoritative request and execute step, verifies
successful complete stdout bytes against their receipt, and accepts one
JSON object containing exactly `verb`, `args`, `plan`, `plan_sha256`.
The verb is the declared plan verb; arguments satisfy the mutation's
existing schema and preserve the discovery target; the plan's exact
bytes must hash to the recorded hash. It rechecks final guards and
episode identity before filing, retaining the proposal and source
request/step/stream on the approval request. Partial output, failed
reads and ambiguous proposals file no approval request.

Largest-fit volume discovery uses this boundary. No whole-GiB growth
that fits remains a native explicit refusal, with full stdout/stderr
and exit retained; no replica is guessed or moved. A filed expansion
request still waits for the named approver's passkey on the ordinary
runner's freshly rendered explicit plan. Report-only PVC intent and
live capacity observations grant no storage authority.

## Authorization

Phase 1 was READ-ONLY. `reclaim-disk` is the FIRST MUTATING verb —
authorized by David, 2026-09-03, after the forge disk filled and blocked
CI for every train. Its bound is by construction of the ONE script it
calls (`infra/forge/disk-floor-sweep.sh`, the same definition the hourly
timer runs — §9a): regenerable docker caches only, in a fixed order,
stopping at the floor, never volumes or non-docker paths, loud non-zero
exit when the floor stays unmet rather than deleting harder. Any further
mutating verb needs the same explicit authorization — that entry is a
precedent for the PROCESS, not a loosened default. A mutating verb says
`MUTATING` in its `about` and names who authorized it; the lint
`the-controls-are-bounded-verbs.sh` derives its roster from that word.

## Shape of a verb file

```json
{
  "about": "what it does, MUTATING if it is, who authorized it",
  "hosts": ["forge"],
  "argv": ["infra/forge/some-script.sh", "{1}"],
  "params": [{"name": "sha", "pattern": "^[0-9a-f]{7,40}$"}],
  "timeout": 120
}
```

`argv` is the exact command, literal words plus `{N}` placeholders;
`{N}` takes param N (1-based) AFTER pattern validation. A param without
`default` is required; `max` is a numeric ceiling applied after the
pattern has proven the value is digits. A param may instead carry
`one_of`: an exact list of reviewed literal WORDS the packet selects
among (no pattern — equality only). Because the word comes from this
file and never from the packet, a literal may lead with `-`
(`publish-github-pr`'s `--check`). With `optional: true` an absent arg
drops its `{N}` word from the argv instead of failing. `timeout`
(seconds) overrides the runner's default for that verb.

`argv[0]` names a script RELATIVE to the repo (`infra/forge/disk-report.sh`);
the runner resolves it against its own checkout, so every managed host
can carry every script, and a bare command (`systemctl`, `df`) stays a
bare command resolved on PATH. An absolute path is refused by the lint
(66077f9c: eleven verbs once baked the forge checkout's path in and
could run nowhere else).

**The tree's own CLI is a bare command too.** Since backlog 9f00a805
(consolidation H8) every managed host installs `/usr/local/bin/boss`
from the converged image (`infra/estate/install-cli-from-image.sh`),
so a verb whose behaviour already exists as a `boss` verb names it
directly — `run-car-probe` is `["boss", "prove", "{1}", "--from-car",
"--unattended"]` — and the shell twin that re-implemented it on the
host is deleted with its pin. The runner hands such a verb its own
account as `BOSS_ACTOR` (the CLI refuses an unnamed write) and the
system of record from its unit's `EnvironmentFile`; the verb's exit
code is the packet's `exit_code`, so a CLI verb run this way must make
its exit the verdict. The remaining twins retire the same way, one
verb per car, each measured first (which of the script's behaviours
the CLI verb lacks — the argument shape, the refusals, the output a
reader of the packet expects, the host-side actions a CLI verb cannot
do from the system of record alone).

`hosts` is WHICH HOSTS THE VERB SERVES — the estate node ids (`nodes.id`
in `infra/postgres/schema`, the same string a packet's `metadata.host`
carries) whose runner will execute it. REQUIRED, and ABSENT MEANS
REFUSE: the runner refuses a verb whose `hosts` does not list its own
`HOST_ID`, so a new verb cannot reach a host by forgetting to say. It is
not a privilege boundary — every runner reads this same directory — it
is the door telling the truth about what it opens. Before 2026-09-11 it
could not: boss-gcp got its runner that day and 11 of the 16 verbs named
a script under the FORGE's checkout (`infra/forge`), which does not
exist there, so standing the runner up advertised a vocabulary of which
11 could only fail on ENOENT. An exec failure is not a verdict
(CLAUDE.md §Diagnosis); a refusal that names the verb, this host and the
hosts that verb does serve is. Widening a verb to another host is a
reviewed change to that verb's file — boss-gcp's set is deliberately the
read-only host-agnostic reads, plus exactly the MUTATING verbs
`infra/lint/a-verb-declares-the-hosts-it-serves.sh` admits BY NAME with
their authorization. The first is `retire-second-stack` (David
2026-09-11, design 9e3e093f): bounded to the unit list the tree carries
at `infra/gcp/second-stack-units.txt`, capture-before-stop, `--dry-run`
exercisable without acting, `--for-real` a human's decision to file.
The second, `uninstall-not-in-role` (same authorization, car 4 of
d5941ef3), carries no list at all: its set is what
`install-units.sh roster` says the host's LIVE roles do not name —
the installer's own derivation — and an empty set is a refusal. The
third, `retire-cloudflared` (David 2026-09-16, design 4c565f8c; backlog
0b7804f3 car 4), retires the host's hand-written tunnel connector —
the unit whose inline token unit-cat once leaked (9c760dd7) — and is
bounded to that ONE unit, named in the script and never a param: it
verifies the hand-over through the system of record BEFORE anything
stops (the newest converge that observed the in-cluster connector must
be under two hours old, `connected`, and routing every hostname
`infra/cluster/instances.toml` declares), prints the unit through
unit-cat's mask, and refuses success while systemd still holds the
unit. It never touches the tunnel in Cloudflare: that is the broker's
revoke phase, which completes on its own once the old tunnel shows
zero connections. The fifth, `publish-drift` (retro 27fad542 approved by
David 2026-09-18; backlog a2f97942), publishes every platform workflow
kind the tree moved ahead of as ONE act — 23 `publish-workflow`
requests were one hand loop on 2026-09-18 — and is bounded by
composition: the drift set is `publish-workflow.sh <kind> --check` per
kind, the publish is that verb per tree-ahead kind, a live row the tree
never said is listed field by field and never published (there is no
`--force-tree`; that stays the Drift tab's approve, one kind at a
time). Its `mode` DEFAULTS to `--check`, which is what lets a
dispatcher rule file it on boss-gcp's checkout moving without any
authority widening: a `jobs.spawn` packet that names no args takes the
default, so it can only ask; `--for-real` is a word a packet carries on
purpose. (A `jobs.spawn` CAN carry an args list since 4d53fae2,
2026-09-22 — so `prune-registry-versions-daily`, the registry delete a
daily rule files unattended (design 97add747, authorised by David
2026-09-27), is a verb of its own by choice rather than necessity: the
authorisation for a delete nobody files lives on a file that can only
be that delete.) The sixth,
`reclaim-gcp-root` (backlog d3c7eada car 2, 2026-09-26; admitted when
David authorises the verb), frees boss-gcp's root: it removes exactly
`/opt/boss-binbak-*` and `/opt/boss-dev-bak` — two globs fixed in the
script, never a param — keeping a checkout (with its unpushed
state in the plan as evidence, 2026-09-27, read with plumbing that
runs nothing the checkout's config names and never as root: as its
owner, a root-owned one as nobody; a partial clone is kept unread,
because a missing object would fetch through its promisor remote) —
except `/opt/boss-dev-bak`, which it removes, first, when the signed
plan proves every checkout in it holds nothing unpushed (no commit on any
ref that only a remote its config does not name, or a remote at a local
path rather than a network URL, holds; no linked worktree, no
submodule repository under `.git/modules`, nothing staged, modified,
untracked or stashed), re-proving it
just before the rm and refusing the whole run with exit 78 on any change
(David, 2026-09-30, backlog d3c7eada) —
refusing a symlink, a backup
changed (ctime) in 30 days, or one that a live tree, a mount, a
symlink, a running process, a loaded unit or a unit file on disk
resolves into, and vacuums the journal to a fixed 1G. It is an
approval verb: its plan verb `plan-a-gcp-root-reclaim` runs every bound
and removes nothing, and each real run needs David's passkey on that
plan. `/var/backups`, homes, `/usr/local`, `/opt/boss` and
`/opt/boss-cli` are data and David's call, out of its reach by
construction — but for two files-by-shape David decided on: the retired
second stack's capture (`second-stack-<stamp>.sql`, backlog f44ca628),
and the dumps the retired boss-gcp off-site leg left in
`/var/backups/boss-cluster-pg` (`boss-<stamp>.sql.gz`, backlog 4bf7bdd1,
2026-10-01: the GCS bucket is the off-site copy), each bound to its
identity in the signed plan. The seventh, `retire-ops-runner` (David 2026-10-01,
design a79a8067; backlog 98eb9349), serves the forge too: it is the
one verb that closes the door it arrives through, so the runner retires
ITSELF — it stops and disables exactly `boss-ops-runner.timer`, a
literal in the script, and never the oneshot service whose pass is
running it, so that pass survives to report. It refuses unless the
host's role is already undeclared on a live read, another host still
declares `ops-runner`, and no other ops-request is open for the host;
it writes `/var/lib/boss/ops-runner.retired` before the stop, and the
host's converge (`install-ops-runner.sh`), owing nothing to the runner,
reports RETIRED (or DISABLED BY HAND, with no marker) and removes the
unit files. Each run needs David's passkey on the plan
`plan-retire-ops-runner` renders.

## Effect — an exit 0 is not a proof

Every MUTATING verb declares how a run SHOWS ITS EFFECT (backlog
fdbb447e part 1, design 3036296f mechanism B, David 2026-09-27).
`answered` says the verb ran and `exit_code` says it did not fail;
neither says that what it exists to change changed — the daily prune's
proof read the outcome alone and counted a refused run as proof. So the
verb file carries exactly one of:

- `effect` — the regex (jq's engine, the runner's) of the line its
  script prints ONLY after it has read back what it changed — a
  re-list, a re-stat, a re-query — or, in a dry-run mode, the line
  saying it changed nothing. Opens `^<name>: `, in the script's own
  voice, and every literal stretch of it is text the script carries.
- `effect_unread` — why the script has no such read-back yet, and what
  would read it. The set of verbs carrying it only shrinks: a new
  MUTATING verb prints its read-back and declares `effect`.

`ops-runner.sh` judges the declaration ONCE, on the run's own output,
and records the verdict on the execute step beside `exit_code`, for an
exit 0 only: `effect` (the last matching line), `effect_unproven` (no
line matched, or the regex could not be judged), or `effect_unread`
(the file's reason, copied, so the run says out loud that exit 0 is all
it proves). Every reader takes that verdict — `boss ops --wait` fails an
exit 0 whose effect was not shown, and `verb_failure` (the answered-
ops-request judges) treats it as a failed verb. Nothing is REFUSED: the
verb has already run, and a verb that cannot prove its effect says so
loudly (DR rule 62dac114). Pinned by
`crates/core/boss-testing/tests/a_mutating_verb_declares_its_effect.rs`
and `ops_runner_sh.rs`.

## Approval verbs

A verb that declares `requires_approval` runs only under a passkey
approval of a rendered plan (design 17835005; the runner half is backlog
fd7090cc). Its file carries three more keys, each checked by the runner
and by `boss ops` before anything is filed or rendered:

- `plan_verb` — a read-only verb here, serving the same hosts, taking
  exactly this verb's params less the last, which prints the plan on
  stdout and `plan-sha256:` on stderr;
- a last param named `plan_sha256`, required, `^[0-9a-f]{64}$` — the
  runner appends sha256 of the SIGNED plan, and the script re-renders
  and refuses bytes that no longer hash to it;
- `approvers` — employee ids, e.g. `["emp-david"]`: only a presence
  stamp whose `authority_id` is on this list approves (design 03451237
  q2, David 2026-09-22: a named list, never a role, because a role is
  registry data and a role gate hangs the approval on whoever can write
  a policy row). Adding an approver is a reviewed change to this file.

A completed approve step is not by itself an approval: Reject runs the
same passkey ceremony and completes it too. The runner runs the write
only when the step's `decision` — saved before the stamp, so inside the
signed shape — is exactly `approved`; ops-request routes any other
decision to `refused` (adversarial re-review of fd7090cc, 2026-09-25).

## Nothing to do — a plan with no change in it asks nobody

A plan verb may declare the regex (jq's engine, the runner's) its WHOLE
plan matches when it names no change:

```json
"nothing_to_do": "\\Awould vacuum the journal to 1G \\(journalctl --vacuum-size=1G\\)\\n\\z"
```

When the plan the runner renders for an approval request matches it,
the runner closes the request through ops-request's `nothing-to-do`
terminal — the plan, its hash, the verb, host, args, the plan verb and
this pattern recorded on that step — and never writes the plan onto the
approve step, so no passkey is asked for a no-op (backlog b2f78bb9, car
3 of 3df309bf: a remedy the machine files again after it already ran
renders exactly such a plan). Anchor it to the whole plan (`\A` … `\z`),
not to a line: a plan can quote text it read off the host, and a line
that text can forge must never close a request. Every doubt asks
instead — a regex jq cannot judge, or a request filed under an
ops-request version without the terminal, is rendered for the passkey
as before. Only a plan verb some approval verb names may declare it;
`ops_runner_approval_sh.rs` (`the_shipped_nothing_to_do_declarations_hold`)
holds the tree to that, refuses a declaration that does not open with
`\A` and close with `\z` or that matches its nothing-plan with a byte
added at either end, and holds each declaration to the plans its verb
renders — a verb that declares one with no plans listed in that test's
`nothing_to_do_plans` is a red gate (backlog aa816dd4).

Only the runner closes a request this way. The `nothing-to-do` step
declares `written_by = "automation:ops-runner"`, and the jobs API
refuses its record from any other automation or agent session; a
person may still write it. A runner refused there — one signing under a
different `BOSS_OPS_ACTOR` — renders the plan for a passkey instead.

## Discovering explicit volume-plan arguments

`plan-the-largest-instance-volume-expansion <namespace> <pvc>` is a
read-only discovery verb. It returns one JSON proposal with `verb`,
explicit `args`, `plan` and `plan_sha256`. The largest whole-GiB size
must fit every assigned replica's disk, counting replicas sharing a
disk, satisfy current physical headroom, and remain within the existing
2x and 100GiB bounds. Missing or ambiguous facts refuse.

The proposal is neither approval nor execution. A later request freezes
its explicit namespace, PVC and size; the existing
`plan-an-instance-volume-expansion` renders that exact plan for David's
passkey, and `expand-instance-volume` requires its signed hash. Live
physical space can change a new discovery result, but admissible changes
leave the already resolved explicit-size plan hash unchanged.

**When no growth fits** (design `ada8f698`, option A — David,
2026-10-07). At the script's ceiling the read refuses: no replica move
changes a ceiling. Otherwise a replica disk is what binds, and the read
asks `move-volume-replica.sh --plan-decisive` whether *exactly one*
replica move would free the volume. A replica is eligible only when the
existing move plan's own bounds hold for the volume and for every disk
the new replica can land on, and every *remaining* replica disk admits
the projected growth by that plan's ledger and live sums. One eligible
replica is proposed — `verb` `plan-a-volume-replica-move`, `args`
`[volume, replica]`, `target` `[namespace, pvc]`, that replica's ordinary
move plan and its hash. None, or several, refuse naming every candidate;
nothing ranks, and listing order never chooses. It also refuses when no
replica disk is short (a move frees nothing) and when the volume holds
more replicas than the retirement floor of 3 (retiring is then as
admissible as moving, and that choice is a person's).

The verb that owns the discovery declares the fallback beside it:
`"no_growth_plan_verb": "plan-a-volume-replica-move"` in its
`discovery_remedies` entry. From such a proposal the dispatcher files
**only that read-only plan request**, with the proposal and its byte
receipt as provenance, and says so on the finding's open alarm. It never
files `move-volume-replica`: that request is a person's to file, and
David's passkey signs its plan.

## Remedies — the machine files the request, the human signs the plan

An approval verb may declare the estate findings it relieves:

```json
"remedies": ["disk_tight:boss-gcp"]
```

Each entry is a finding keyed exactly as `estate.alarm` keys its alarm
packet (`<class>:<id>` — `disk_tight:<host>`, `gone:<node>`,
`unit_unhealthy:<host>/<unit>`, …). When an estate comparison carries
that finding, the dispatcher rule
`file-the-remedy-a-verb-declares-for-an-estate-finding` (handler
`ops.file_remedies`, which compiles this directory in) files the verb's
ops-request itself — `host` the verb's one host, `args: []`,
`requires_approval: true`, subject `<verb>@<host>` — so the approve step
reaches the named approvers with the plan rendered, and nobody types a
command (backlog 3df309bf: "a human act is a signature on rendered
bytes, never a transcription"). At most one open request per subject,
and none refiled while one was opened in the last week, whatever became
of it: a remedy that ran need not clear its finding. A request whose
approver DECLINED its plan is not refiled until the estate observes the
finding clear — its alarm closed after the decline, by the recover rule
or by a person whose close is not `duplicate` or `decline` — because a
decline is an answer (backlog b2f78bb9; a person's close counts since
aa816dd4). While it holds, the open alarm carries
`remedy_held:<verb>@<host>` saying so, and the request filed once it
lifts carries `decline_lifted`, naming the declined request and the
alarm whose close lifted it. And a plan verb that
declares `nothing_to_do` closes a request whose plan names no change
before anyone is asked (below).

Filing grants nothing, so it is the machine's; but the handler files
ONLY a verb that runs under a passkey. A `remedies` declaration is
refused unless the verb declares `requires_approval`, a `plan_verb` and
`approvers`, takes `plan_sha256` as its ONLY param (the machine has
nothing to fill any other with), and serves exactly one host; a class
the alarm never keys is refused too, because nothing could ever file it.
The handler's test `every_declared_remedy_is_a_passkey_gated_verb` holds
this directory to that, so a bad declaration is a red gate.

## Defense

Stated once and relied on by `ops-runner.sh`: the runner never executes
a packet-supplied string. It builds an argv ARRAY from the verb file —
no `sh -c`, no `eval`, no interpolation into program text. The patterns
admit no whitespace and no leading `-`, so a validated arg can neither
split into extra words nor be parsed as an option; `unit-status` also
passes `--` so even a future pattern loosening cannot turn an arg into a
flag there. An arg carrying any control character is refused before its
pattern is consulted: jq's `$` also matches before a trailing newline,
so `"word\n"` passed `^[a-z]+$` and grew the argv an empty word
(security review of fd7090cc, 2026-09-24).

JSON rather than TOML because the runner is sh + jq (directive 26d61c97:
no python) and jq reads JSON natively — a hand-rolled TOML parser in sh
is exactly the fragile string handling this protocol exists to ban.
