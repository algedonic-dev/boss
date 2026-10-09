#!/usr/bin/env bash
# preflight: serial — reads the live protocol registry off the jobs API through lib/sor-read.sh, waiting out a roll; one reader of the record per pre-flight
# consist: skip — compares the LIVE registry to a tree, so its answer is a fact about the estate, not a function of the sha a gate vouches for; the live run belongs after the converge (design d349e0ba)
#
# WHY NO GATE RUNS THIS (design d349e0ba, backlog b79054b2, 2026-09-28).
# Until then it was in the pre-flight roster, and the roster test
# (`a_lint_that_scanned_nothing_is_red.rs`) ran it a second time inside
# `test`. Gate 2f82e7fa went red on both: train #782 landed
# `break-glass-enrolment`, the converge seeded it live, and a car whose
# base predated #782 had no file for it — a live kind the tree under
# test could not author, so the car's BASE decided its verdict. The
# comparison is right; the moment was wrong. After a merge has
# converged, tree, image and registry are supposed to agree, so that is
# where the design runs it, filing each disagreement as work. The
# self-test and the static half still run on every scoped gate: the
# tree-wide pin `a_lint_that_reads_the_api_waits_out_a_roll.rs` runs
# this script on the tree against a refused registry and wants exit 3,
# so a static finding (exit 1) reds it.
#
# the-live-protocols-are-the-authored-protocols — every protocol the
# running registry admits Jobs under is one this tree writes down.
#
# WHY THIS EXISTS
# ---------------
# The sibling one layer down is
# `infra/lint/the-live-rules-are-the-authored-rules.sh`, and this is the
# same defect in the same shape: a registry whose rows can be authored
# live through the API, a tree that is supposed to describe them, and
# nothing comparing the two. Measured 2026-09-10 against the live
# registry: EIGHTY-FOUR admitted workflow kinds, and TWELVE of them had
# no authored source anywhere in the tree — not a bundle file, not a
# tenant seed, not a Rust literal, not a migration. They were published
# through `POST /api/workflows` + publish and never written back.
#
# Twelve is not a list of curiosities. `gate-run` is the protocol every
# car's gate packet runs on; `publish-request` is how a branch from a
# credential-less workspace reaches the forge; `maintenance-sweep` is
# the chore family seven dispatcher clock rules spawn. Answering "what
# does the gate-run protocol require" meant querying production, a
# change to any of them was a live publish with no diff and no second
# reader, and a deployment built from this tree did not have them at
# all.
#
# NOT A COMPLAINT THAT PROTOCOLS ARE DATA. CLAUDE.md is explicit and
# right: "Adding a new workflow means adding a Workflow row, not
# touching core code", and the three-layers frame says a protocol that
# cannot be replaced without a deploy has leaked into the substrate.
# All of that stays true. The property here is narrower and does not
# contradict it: whatever the registry admits, the tree can SHOW you.
# Publishing a new version live stays legal; leaving the tree unable to
# describe it does not.
#
# THE CHECKED PROPERTY
# --------------------
# Live kinds are a subset of the kinds this TREE authors, plus the
# exemptions named below (empty, and meant to stay that way). Read from
# THIS tree deliberately, not from anything the deployment computes,
# because the tree under test is the one a car changes.
#
# FOUR AUTHORING HOMES, all of which count. A kind is authored if the
# tree says what it is, in any form a reader can read and a reviewer can
# diff:
#
#   1. infra/platform/workflows/<kind>.toml — the platform bundle, and
#      the destination. One kind per file, the stem IS the kind (the
#      loader refuses anything else; `platform_bundle.rs` pins it), so
#      `ls` answers "which kinds".
#   2. examples/<tenant>/seeds/workflows.toml — a tenant's bundle, one
#      file of `[[workflow]]` blocks, published by that tenant's prepare
#      step.
#   3. `platform_workflows()` in crates/core/boss-jobs/src/registry.rs —
#      Rust literals, reconciled into the registry on every boot. Being
#      retired into (1) by protocols-as-data; still authored until then.
#   4. An `INSERT INTO workflows` in infra/postgres/schema/ — how
#      `repair-a-train` arrived, and the reason this script reads
#      migrations at all. The packet that filed this gap first counted
#      seventeen and corrected itself to sixteen on exactly this check.
#
# Homes 3 and 4 are duplication this tree is moving away from, not
# endorsements. They count here because the question is "can a reader
# find out what this protocol is", and in both cases they can.
#
# ONE DIRECTION, on purpose. A kind authored with no live row is the
# EXPECTED state in two ways, neither of which is drift: between a
# protocol car's merge and the converge + seed behind it, and for every
# tenant bundle a deployment does not run (this tree ships two tenants;
# a deployment runs one). Failing on it would red cars for windows they
# do not control — the "an infrastructure refusal is not a consist
# failure" cost CLAUDE.md records. So it is REPORTED, never failed on.
#
# WHEN THE REGISTRY IS UNREACHABLE it SKIPS, loudly, and exits 3 —
# `LINT_CANNOT_ANSWER`, lib/git-answer.sh's word for "the machine could
# not answer": never 0, because a skip prints no `scanned` line and a
# clean exit with no count is exactly what lib/scanned.sh refuses; never
# 1, because nothing about the BRANCH was judged. Until 2026-09-18 it
# exited 0 on the argument that the gate ran on the forge host with no
# route here — no longer true (the gate runs in-cluster, design
# 128b5496) — and the day the system of record was rolling under a
# converge, the lint-of-lints pin redded gate 35f4ff0c for a lint that
# had scanned nothing (backlog a26f92c4). gate.sh maps exit 3 to a
# REFUSAL receipt (`refused`, not `failed`), which the conductor
# relaunches and strikes no car for. The static half below still runs,
# so a skip is never a no-op. What it must never do is treat "could not
# read" as "nothing to report": a wrong target
# answers instead of erroring (CLAUDE.md §Doors), so an answer that
# parses but is not an array of workflow rows is a FAILURE, and so is an
# EMPTY one — a registry admitting zero kinds is dead air, not a clean
# bill.
#
# UNLESS THE RUN DECLARES NO ESTATE. `BOSS_ESTATE=none` (the public
# mirror's workflow, and only it) says there is no registry to read: the
# static half runs and its findings still fail, the live comparison is
# not attempted, the lint says so with lib/no-estate.sh's marker and
# exits 0, and the gate names it on the receipt. Unset, the refusal above
# stands; `--require-live` never consults it (backlog 3e63662c).
#
# THE SECOND CHECKED PROPERTY — a live row still SAYS what its file says
# ----------------------------------------------------------------------
# The property above is about existence: whatever the registry admits,
# the tree can show you. It is satisfied by a file that exists and is
# WRONG, and on 2026-09-10 one was. `design-doc-review`'s live v1
# `description` described `boss-docs-api` parsing `### Qn:` headings,
# `/api/design/pending-decisions` and the flush jobs — every one of them
# deleted that day — while the tree file carried the corrected text.
# TWO mechanisms each declined to close the gap: the bundle seed is
# insert-if-missing, so a present row is skipped whole, and
# `bootstrap_reconcile`'s `kind_body_matches` excludes `description` as
# cosmetic, so it saw no drift and republished nothing (backlog
# e882b74c). The row was corrected by an operator publish at 02:53Z on
# 2026-09-11; this half is the mechanism that names the next one.
#
# THREE FIELDS, all scalar strings an operator reads and none of which
# changes what the protocol does: `description`, `label`, `category`.
# `owning_team` is deliberately out — the loader overrides the file's
# key, so a disagreement there could never be cleared by a publish. The
# comparator's own comment carries the reason for each inclusion and
# each exclusion.
#
# AND, SINCE 2026-09-15, FOUR STEP FACETS: the step count, the ordered
# title list, and each step's required-field set and label
# (`title_template`). Structural fields were out on the argument that a
# live row legitimately leads its file between a publish and the car
# that writes it down — true, and the reason drift is REPORTED rather
# than failed on, not a reason to leave it unmeasured. The measured
# cost of not looking (backlog 0ccf23ec): ship-a-change's live v31 had
# a `settled` step and a required `proof` field its file lacked, the
# daily measurement said "one description adrift", and publishing the
# file over the row — the obvious fix for the drift it DID report —
# would have deleted both. Predicates, kinds, field types and
# metadata_defaults stay out; they need the publish path's
# normalisation before an equality means anything. The step's `agent`
# block joined the facets on 2026-09-19 (backlog 1b847556): it is
# structural like `fields`, and a live row lacking it read "agree".
#
# AND, SINCE 2026-09-28, EVERY OTHER KEY (backlog 462cdfe3). The item
# said rotate-a-credential was "v4 in the tree and v2 live" and asked
# why drift had not named it. It had nothing to name: a workflow file
# declares no version (the registry assigns max+1), the "v3"/"v4" in
# that file's header are revisions of its prose, and live v2 (published
# 2026-09-26 07:13Z) matched the file key for key. But the question
# found the real gap one key over: incident's file said
# `surfaces = ["system-incidents"]` and publish-request's
# `owner_role = "platform-admin"`, both landed by cars, while the live
# rows still said `system-design` and `shift-lead` — tree revisions the
# registry never took, reading "56 live rows agree". Workflow `metadata`
# was not compared, nor optional fields, predicates, step kinds or
# procedures, on the normalisation argument above. Measured that day
# over all 56 kinds, only `authority_role` needed any (the publish path
# fills it when a step names none); every other key compared verbatim.
# So every key both copies can state is compared under its own name,
# and the publish-filled one only when the file names it. The file
# still carries no version, so a finding names the LIVE version it was
# measured against; the tree side of it is the file at this checkout.
#
# NOT A CASE FOR WIDENING `kind_body_matches`. That function governs
# every bootstrap-created row, so widening it would change reconcile's
# behaviour for rows this problem is not about — and since
# `platform_workflows()` went empty on 2026-09-11 it iterates nothing,
# so widening it would compare nothing here either. Worth being exact:
# `label` and `category` ARE already in it and `description` alone is
# not, so for a bundle-authored kind nothing compares any of the three.
# That is the gap this half fills.
#
# A DRIFT IS REPORTED, NOT FAILED ON, under a bare invocation — the same
# tolerance the kind half gives the same window, read one field deeper:
# a car edits a description, merges, and the row does not move until an
# operator publishes. Failing would red every car in between, which is
# the churn argument that excluded the field from reconcile arriving
# again as a red gate. `--require-live` is the mode for a caller that
# can act on a verdict.
#
# Usage:  infra/lint/the-live-protocols-are-the-authored-protocols.sh
#           [--require-live] [--report-json <file>] [--self-test]
#         infra/lint/the-live-protocols-are-the-authored-protocols.sh
#           --compare <bundle_dir> <live_json>
#
#   --require-live  For a caller with somewhere to put the answer (a
#                   sweep, a cadence, an operator asking the question
#                   directly). The live comparison MUST happen: an
#                   unreachable registry exits 75 instead of skipping to
#                   0, and a field drift is a verdict (2) rather than a
#                   report. A check that passes when it could not read
#                   is worse than no check.
#   --report-json   Write the SAME facts the text report prints, as one
#                   JSON object, to <file> — for a caller that files
#                   them rather than reads them (infra/protocol-drift.sh,
#                   the daily measurement on boss-gcp; backlog 19dec171).
#                   The verdict is unchanged by this flag. The file is
#                   written only when the live comparison RAN: a skip
#                   writes nothing and says so, because a report of no
#                   comparison would read as "no drift". Its shape is
#                   `write_report`'s comment below.
#   --self-test     Run the fixture cases and say what they proved.
#                   They run on every invocation regardless; the flag
#                   only makes them speak.
#   --compare       Run ONLY the field comparator: <bundle_dir> against
#                   the live answer saved at <live_json>, printing its
#                   COUNTS / DRIFT / ABSENT / NOROW lines and exiting
#                   with its own codes (above `fields_report`). No
#                   network, no self-test, no static half. It exists so
#                   boss-testing/tests/the_drift_facets_are_one_list.rs
#                   can run this comparator and `boss tenant publish`'s
#                   over one bundle and one answer (backlog e5dc276d).
#
#   Exit codes:  0 clean (or drift, reported, bare invocation; or the
#                  static half clean under BOSS_ESTATE=none)
#                1 a failure of the tree — an unauthored live kind, a
#                  Rust literal, a stale exemption, an unreadable
#                  bundle file, or a comparison refused as vacuous
#                2 field drift, under --require-live only
#                3 LINT_CANNOT_ANSWER — the live comparison could not
#                  run (bare invocation; the gate records a refusal)
#               64 unknown argument
#               75 EX_TEMPFAIL — the same condition under
#                  --require-live, the code infra/protocol-drift.sh reads
#
#   BOSS_JOBS_URL  read surface base (default: the in-cluster machine
#                  door, boss-jobs-internal:7900)

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. infra/lint/lib/git-answer.sh || exit 3
# The live read waits out a rollout before it refuses (backlog 834ddb7c).
# shellcheck source=infra/lint/lib/sor-read.sh
. infra/lint/lib/sor-read.sh || exit 3
# BOSS_ESTATE=none skips the live half, and only it (backlog 3e63662c).
# shellcheck source=infra/lint/lib/no-estate.sh
. infra/lint/lib/no-estate.sh || exit 3

BUNDLE="infra/platform/workflows"
# The kinds held out of the publish, and their one reader — asked before
# this report names a publish for any drifting kind (backlog c6bd9f18).
HOLDS_REL="infra/platform/workflow-holds"
HOLDS_PY="infra/gcp/workflow-holds.py"
TENANT_GLOB="examples/*/seeds/workflows.toml"
REGISTRY_RS="crates/core/boss-jobs/src/registry.rs"
SCHEMA_DIR="infra/postgres/schema"
BASE="${BOSS_JOBS_URL:-http://boss-jobs-internal.boss.svc.cluster.local:7900}"
URL="$BASE/api/workflows"

# Kinds the LIVE registry admits with no authored source anywhere, each
# with the reason it is tolerated. A set of names rather than a count,
# so adding a protocol never edits this line and two cars cannot collide
# on it (the BASELINE=<n> lesson, §9a).
#
# EMPTY, and that is the point. The twelve this script was written for
# were BACKFILLED in the same change that added it, because a lint that
# lands red blocks every car until someone drains it — the way
# `the-live-rules-are-the-authored-rules` held every rule-retirement car
# out of the queue until it learned to read retirement migrations. An
# entry here is a decision to stop describing one protocol, and an
# exemption list is how this class of gap got here in the first place.
EXEMPT=()

NAME="the-live-protocols-are-the-authored-protocols"

# How many kinds the field comparison must actually compare before its
# silence means anything. See the floor's own comment below.
FIELD_FLOOR=20

REQUIRE_LIVE=0
SELF_TEST=0
REPORT_JSON=""
COMPARE_BUNDLE=""
COMPARE_LIVE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --require-live) REQUIRE_LIVE=1 ;;
        --report-json)
            [ -n "${2:-}" ] || { echo "$NAME: --report-json needs a file path" >&2; exit 64; }
            REPORT_JSON="$2"; shift ;;
        --self-test)    SELF_TEST=1 ;;
        --compare)
            [ -n "${2:-}" ] && [ -n "${3:-}" ] || { echo "$NAME: --compare needs <bundle_dir> <live_json>" >&2; exit 64; }
            COMPARE_BUNDLE="$2"; COMPARE_LIVE="$3"; shift 2 ;;
        *) echo "$NAME: unknown argument: $1" >&2; exit 64 ;;
    esac
    shift
done

fail() { echo "the-live-protocols-are-the-authored-protocols: $*" >&2; problems=$((problems + 1)); }
problems=0

# ---------------------------------------------------------------------------
# The field comparator, and the self-test that proves it can refuse.
# ---------------------------------------------------------------------------
# `fields_report <bundle_dir> <live_json>` is the whole of the second
# property, factored out so it can be driven from fixtures with no
# network. It prints one machine-readable line per finding:
#
#   COUNTS  parsed=<n>  compared=<n>  drifted=<n>
#   DRIFT   <kind>  <field>  v<live_version>  at=<offset>  tree=<len>  live=<len>  <tree window>  <live window>
#   ABSENT  <kind>  <field>          the FILE makes no claim
#   NOROW   <kind>                   no ACTIVE live row for this kind
#
# Exit codes are the vocabulary the self-test asserts against, because a
# comparator that cannot say WHICH way it failed sends the next reader
# to re-derive it (CLAUDE.md §Diagnosis, "a verdict must name what
# failed"): 0 compared cleanly or with drift, 3 the live answer did not
# parse, 6 a bundle file did not parse or held no [[workflow]], 7 the
# non-vacuity floor refused.
fields_report() {
    python3 - "$1" "$2" "$FIELD_FLOOR" <<'PY'
import json, sys, tomllib, pathlib

bundle_dir, live_path, floor = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])

# The fields compared, and the reason each one is on the list. All four
# are SCALAR STRINGS an operator reads, and none of them changes what
# the protocol DOES — which is exactly why nothing else compares them.
#
#   description  the sentence an operator reads to know what a protocol
#                is for. The measured instance: design-doc-review's live
#                v1 described `boss-docs-api`, `/api/design/pending-
#                decisions` and the flush jobs, all deleted on
#                2026-09-10, so the row sent a reader looking for a
#                service that is gone (backlog e882b74c). It is also
#                the ONE field `kind_body_matches` names as cosmetic and
#                skips.
#   label        the protocol's name in every list and tab that renders
#                it. A label disagreeing with its file means an operator
#                and a reviewer are talking about differently-named
#                things.
#   category     groups protocols in the UI, and has a measured defect
#                of its own: `maintenance_spec` put the DESCRIPTION in
#                the category column for all three chores (6c796f75).
#
# `owning_team` WAS on this list and came off it, which is worth keeping:
# the file's key is decorative. `seed_loader` ends its conversion with
# `spec.owning_team = default_owner` — "platform" for this bundle, the
# tenant id for a tenant's — so the TOML key is read and thrown away,
# and `boss workflow publish` reads the same loader. A disagreement
# there is therefore not a row lagging its file; it is a file claiming
# something the loader will not honour, and NO publish could ever clear
# it. A finding no action can close trains a reader to skip the whole
# report (§Diagnosis, "a check nobody reads").
#
# UNTIL 2026-09-28 this paragraph listed `subject_kinds`,
# `metadata_schema`, `entitlements`, `metadata` and the rest of `steps`
# (predicates, kinds, field types, metadata_defaults) as DELIBERATELY
# NOT COMPARED, because they would "need the publish path's
# normalisation before an equality means anything". Measured over all
# 56 compared kinds, that was true of one key, `authority_role`, and
# false of the others — and leaving them out hid two live rows lagging
# their files (backlog 462cdfe3). They are compared now, each under its
# own name, by the EVERY OTHER KEY block below the step facets. Still
# out: `version`, `status`, `created_at`, `authoring_job_id` — four
# columns with no TOML key at all, so the file cannot disagree with
# them — and `owning_team`, for the reason above.
FIELDS = ("label", "description", "category")

# THE STEP FACETS, compared since 2026-09-15 (backlog 0ccf23ec). Steps
# were on the not-compared list above, for the reason it still gives
# about normalisation — and the reason was true of `ready_when` and
# false of everything the live row and the file both state VERBATIM.
# The measured cost of leaving them out: ship-a-change's live v31
# carried a `settled` outcome step, a required `proof` field on
# `proven` (the machine-probe rule, v22) and a procedure on every step,
# while its file had eight steps and no `proof`; the daily measurement
# read "one description adrift" and said nothing about the step, so
# "fix the drift" — publishing the file over the row — would have
# DELETED the step and the requirement that makes proven a fact. Each
# facet is rendered on both sides as one scalar string, so it rides the
# same DRIFT line, the same windows and the same JSON report as a
# description, and a reader learns WHICH step and WHAT differs:
#
#   steps.count                 how many steps — the headline number
#   steps.titles                the ordered title list; a step on one
#                               side only shows up here, once, rather
#                               than as a per-step line against nothing
#   steps.<title>.required      the sorted names of that step's
#                               required fields — the completion
#                               contract; `proof` is the worked case
#   steps.<title>.optional      the sorted `name:type` of every field
#                               NOT required (2026-09-28, 462cdfe3):
#                               rotate-a-credential's `old_token` was
#                               one, and `required` cannot see it
#   steps.<title>.title_template  the step's label as rendered; the
#                               same class as `label` one level up,
#                               and where pr-train's live v17 (yard
#                               grammar) differed from its file with
#                               nothing else to show for it
#   steps.<title>.agent         WHO runs the step (design c87fb59b car
#                               1): the block's keys as canonical JSON,
#                               `<absent>` when neither side has one.
#                               Added 2026-09-19 (backlog 1b847556):
#                               measured 2026-09-18 23:25Z, the live
#                               backlog-item v2 had no block on any
#                               step, its file had two, and this read
#                               "54 live rows agree with their file".
#                               Numbers compare as floats — the file
#                               spells `budget_usd = 5`, the registry
#                               hands back 5.0 through AgentSpec's f64
#                               — the same rule the publish path's
#                               step_view applies (publish-workflow.sh,
#                               #464). Held out for one day after that
#                               fix so a landing did not red every gate
#                               before the publish behind it; the loop
#                               closed on 2026-09-19 (live v3 carries
#                               both blocks).
#
# Per-step facets are compared only for titles BOTH sides hold, so a
# missing step is one finding (in `steps.titles`), not one per facet.
# Same tolerance as every other field here: reported, never failed on
# under a bare invocation, a verdict under --require-live.
def step_facets(steps):
    steps = steps if isinstance(steps, list) else []
    titles = [str(s.get("title", "")) for s in steps if isinstance(s, dict)]
    out = {"steps.count": str(len(titles)), "steps.titles": ",".join(titles)}
    for s in steps:
        if not isinstance(s, dict):
            continue
        t = str(s.get("title", ""))
        fields = s.get("fields") if isinstance(s.get("fields"), list) else []
        required = sorted(
            str(f.get("name", "")) for f in fields
            if isinstance(f, dict) and f.get("required") is True
        )
        out[f"steps.{t}.required"] = ",".join(required)
        out[f"steps.{t}.optional"] = ",".join(sorted(
            f"{f.get('name', '')}:{f.get('field_type', '')}" for f in fields
            if isinstance(f, dict) and f.get("required") is not True
        ))
        out[f"steps.{t}.title_template"] = str(s.get("title_template", ""))
        out[f"steps.{t}.agent"] = agent_facet(s.get("agent"))
        for key, value in s.items():
            if key not in STEP_NAMED:
                out[f"steps.{t}.{key}"] = canon(value)
    return out, titles

# EVERY OTHER KEY, since 2026-09-28 (backlog 462cdfe3). The list above
# was curated, and a curated list answers only the questions its author
# thought of: rotate-a-credential's file-side "v3" added an OPTIONAL
# field and a procedure, incident's file moved the protocol to another
# surface, publish-request's changed its owner role — and every one of
# them read "agree", because none touched a required field, a title or
# a label. The reason those keys were out ("they need the publish
# path's normalisation before an equality means anything") was
# MEASURED that day against all 56 compared kinds: predicates, step
# kinds, field types, metadata_defaults, terminals, audiences,
# subject_kinds, metadata, metadata_schema and entitlements all compare
# verbatim, once "the file is silent" and "the row holds null / false /
# an empty list" are read as the same claim and a number is read as a
# float (TOML 2, registry 2.0 — the agent block's rule). ONE key did
# not: `authority_role`, which the publish path fills from the owner
# role when a step names none (13 steps that day), so it is compared
# only when the FILE names one. Every other key is compared under its
# own name, so a key added to a step tomorrow is compared the day it
# is written rather than the day someone remembers this list.
#
# THESE LISTS LIVE TWICE, and a test holds them equal (backlog
# e5dc276d). `boss tenant publish` asks the same question through
# `boss_jobs::bootstrap::workflow_changes`, whose constants are these
# five lists under their Rust names; that copy lacked `agent` for nine
# days and every key above for one, because a comment said "the SAME
# list" and nothing checked it. `boss-testing/tests/the_drift_facets_
# are_one_list.rs` reads each `NAME = ` line below and compares it entry
# by entry, and runs both comparators over the real bundle through
# `--compare`. Keep each list on ONE line that starts with its name.
NAMED_STEP_FACETS = ("required", "optional", "title_template", "agent")
STEP_NAMED = {"title", "title_template", "agent", "fields"}
ROW_ONLY = {"kind", "version", "status", "created_at", "authoring_job_id", "owning_team", "step", "steps"}
FILLED_WHEN_SILENT = {"authority_role"}

def canon(value):
    """One canonical string for a structural value; None for no claim.

    Absent, null, false, "" and an empty list or table are one claim —
    the publish path writes the default the file was silent about.
    """
    if value is None or value is False or value in ("", [], {}):
        return None
    def norm(x):
        if isinstance(x, bool):
            return x
        if isinstance(x, (int, float)):
            return float(x)
        if isinstance(x, dict):
            return {k: norm(v) for k, v in x.items()}
        if isinstance(x, list):
            return [norm(v) for v in x]
        return x
    return json.dumps(norm(value), sort_keys=True, ensure_ascii=False, default=str)

def agent_facet(block):
    """The agent block as one canonical string, or None for no block.

    Every numeric value is rendered as a float so the TOML integer and
    the JSON float the registry hands back for it compare equal; a
    bool is not a number here (Python's is), so it is left alone.
    """
    if not isinstance(block, dict):
        return None
    canon = {
        k: (float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else v)
        for k, v in block.items()
    }
    # AgentSpec's legacy default, pinned to the Rust comparator over the
    # real bundle. Explicit verified/null/unknown values and other keys
    # remain findings; this does not supply an executor attestation.
    canon.setdefault("executor_provenance", "advisory")
    return json.dumps(canon, sort_keys=True, ensure_ascii=False)

def facets_to_compare(tree_facets, live_facets, tree_titles, live_titles):
    both = set(tree_titles) & set(live_titles)
    yield "steps.count"
    yield "steps.titles"
    for t in tree_titles:
        if t in both:
            named = [f"steps.{t}.{k}" for k in NAMED_STEP_FACETS]
            yield from named
            rest = {
                f"steps.{t}.{k}"
                for side in (step_keys(tree_facets, t), step_keys(live_facets, t))
                for k in side
            }
            yield from sorted(rest - set(named))

def step_keys(facets, title):
    """The per-step keys one side states for `title`, named facets excluded."""
    prefix = f"steps.{title}."
    return [k[len(prefix):] for k in facets if k.startswith(prefix)
            and k[len(prefix):] not in NAMED_STEP_FACETS]

try:
    doc = json.load(open(live_path))
except Exception as e:
    print(f"the live answer did not parse: {e}", file=sys.stderr)
    sys.exit(3)
rows = doc.get("workflows") if isinstance(doc, dict) else doc
if not isinstance(rows, list):
    print("the live answer is not an array of workflow rows", file=sys.stderr)
    sys.exit(3)
active = {
    r["kind"]: r
    for r in rows
    if isinstance(r, dict) and "kind" in r and r.get("status", "active") == "active"
}

def window(s, at, width=90):
    """The text around the first difference, on one line.

    Both full copies stay readable at named locations — the file in
    this tree, the row at GET /api/workflows — so this reduction
    discards no only-copy (§Diagnosis). It exists so a 1,200-character
    description does not make the finding unreadable.
    """
    if s is None:
        return "<absent>"
    start = max(0, at - 20)
    cut = s[start:start + width]
    cut = cut.replace("\t", " ").replace("\n", "\\n").replace("\r", " ")
    return ("…" if start else "") + cut + ("…" if start + width < len(s) else "")

def first_diff(a, b):
    a, b = a or "", b or ""
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b))

def drift_line(kind, field, row, tree, live):
    """One DRIFT finding — the same shape for a field and a step facet."""
    at = first_diff(tree, live)
    return "DRIFT\t{}\t{}\tv{}\tat={}\ttree={}\tlive={}\t{}\t{}".format(
        kind, field, row.get("version", "?"), at,
        len(tree) if isinstance(tree, str) else "-",
        len(live) if isinstance(live, str) else "-",
        window(tree if isinstance(tree, str) or tree is None else str(tree), at),
        window(live if isinstance(live, str) or live is None else str(live), at),
    )

files = sorted(bundle_dir.glob("*.toml"))
parsed = compared = drifted = 0
lines = []
for f in files:
    try:
        with open(f, "rb") as fh:
            body = tomllib.load(fh)
        blocks = body["workflow"]
        if not isinstance(blocks, list) or not blocks:
            raise KeyError("workflow")
    except Exception as e:
        print(f"{f}: not a readable [[workflow]] file: {e}", file=sys.stderr)
        sys.exit(6)
    for wf in blocks:
        kind = wf.get("kind")
        if not kind:
            print(f"{f}: a [[workflow]] block with no kind", file=sys.stderr)
            sys.exit(6)
        parsed += 1
        row = active.get(kind)
        if row is None:
            # Not drift, and not counted as compared: the tree leading
            # the deployment is the expected window between a protocol
            # car's merge and the converge + seed behind it.
            lines.append(f"NOROW\t{kind}")
            continue
        compared += 1
        for field in FIELDS:
            if field not in wf:
                lines.append(f"ABSENT\t{kind}\t{field}")
                continue
            tree, live = wf[field], row.get(field)
            if tree == live:
                continue
            drifted += 1
            lines.append(drift_line(kind, field, row, tree, live))
        # Every other workflow-level key, under its own name (see
        # ROW_ONLY and canon above): metadata, subject_kinds,
        # metadata_schema, entitlements, and whatever is added next.
        for key in sorted((set(wf) | set(row)) - ROW_ONLY - set(FIELDS)):
            tree, live = canon(wf.get(key)), canon(row.get(key))
            if tree == live:
                continue
            drifted += 1
            lines.append(drift_line(kind, key, row, tree, live))
        # The TOML key is `step` ([[workflow.step]]); the row's is `steps`.
        tree_facets, tree_titles = step_facets(wf.get("step"))
        live_facets, live_titles = step_facets(row.get("steps"))
        for facet in facets_to_compare(tree_facets, live_facets, tree_titles, live_titles):
            tree, live = tree_facets.get(facet), live_facets.get(facet)
            if tree == live:
                continue
            if tree is None and facet.rsplit(".", 1)[-1] in FILLED_WHEN_SILENT:
                continue
            drifted += 1
            lines.append(drift_line(kind, facet, row, tree, live))

print(f"COUNTS\tparsed={parsed}\tcompared={compared}\tdrifted={drifted}")
print("\n".join(lines)) if lines else None

# THE NON-VACUITY FLOOR. A comparison of nothing is the failure mode
# this whole check is written against: an empty bundle, a reader whose
# idiom moved, or a registry answering about a different world all
# produce "no drift found", which reads exactly like a clean bill
# (backlog 024c0db2; and the falsely-passing absence assertion of
# 61085a9e). The floor is deliberately far below the bundle's real size
# so that adding or retiring a protocol never edits it — it is a
# parse-sanity floor, not a ratchet with a number to bump (§9a).
if parsed != len(files):
    print(
        f"REFUSED\tread {parsed} [[workflow]] block(s) from {len(files)} file(s) — "
        "one kind per file is pinned by the loader, so the reader is broken",
        file=sys.stderr,
    )
    sys.exit(7)
if compared < floor:
    print(
        f"REFUSED\tcompared {compared} kind(s), floor is {floor} — "
        "a comparison this small proves nothing, whatever it found",
        file=sys.stderr,
    )
    sys.exit(7)
PY
}

# ---------------------------------------------------------------------------
# The JSON report — the text report's facts, in a shape a caller can FILE.
# ---------------------------------------------------------------------------
# `write_report <file> <verdict>` reads the same variables the text
# report below prints from — `live_kinds`, `authored`, `unauthored`,
# `EXEMPT`, `fields_out` (the comparator's TSV lines), `pending`,
# `tenant_lines`, `problems` — so the two cannot say different things:
# one source, two renderings (CLAUDE.md §9a). Nothing is re-derived and
# nothing is reduced that the text does not also reduce: the DRIFT
# windows are the same 90-character excerpts, with both full copies at
# their named homes (the file in the tree, the row at GET /api/workflows).
#
# Written only when the live comparison RAN. Under a skip there is no
# report, and the skip says so — a report carrying "0 drifted" from a
# comparison that never happened is exactly the confident wrong answer
# the floor in fields_report refuses (§Doors). The shape:
#
#   { at, target, verdict, problems,
#     live:     { admitted },                 what the registry admits
#     authored: { count },                    what this tree writes down
#     exempt:   [kind…], unauthored: [kind…], live kinds no file authors
#     fields:   { parsed, compared, drifted,
#                 drift:  [{kind, field, live_version, at, tree_len,
#                           live_len, tree_window, live_window}…],
#                         `field` is a compared field (`description`),
#                         a workflow key (`metadata`, `subject_kinds`…)
#                         or a step facet (`steps.count`, `steps.titles`,
#                         `steps.<title>.required`, `.optional`,
#                         `.title_template`, `.agent`, or any other key
#                         of the step: `.ready_when`, `.metadata_defaults`…)
#                 absent: [{kind, field}…] }, the file makes no claim
#     pending:  [kind…],                      authored, not yet admitted
#     tenants:  [{file, not_admitted, total}…] }
#
# `verdict` is the exit code THIS invocation goes on to exit with, so a
# caller filing the report can record it without parsing stderr — and
# so a report from a bare run (drift exits 0) and one from --require-live
# (drift exits 2) are distinguishable on the record.
write_report() { # <file> <verdict>
    local out="$1" verdict="$2" t
    t=$(mktemp -d) || { echo "$NAME: no writable temp dir, so the JSON report could not be assembled" >&2; return 1; }
    printf '%s\n' "$live_kinds"   > "$t/live"
    printf '%s\n' "$authored"     > "$t/authored"
    printf '%s\n' "$unauthored"   > "$t/unauthored"
    printf '%s\n' "$fields_out"   > "$t/fields"
    printf '%s\n' "$pending"      > "$t/pending"
    printf '%s\n' "$tenant_lines" > "$t/tenants"
    printf '%s\n' ${EXEMPT[@]+"${EXEMPT[@]}"} > "$t/exempt"
    python3 - "$t" "$out" "$URL" "$verdict" "$problems" <<'PY' || { rm -rf "$t"; return 1; }
import json, pathlib, sys, datetime

t, out, url, verdict, problems = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])

def lines(name):
    return [l for l in (t / name).read_text().split("\n") if l != ""]

def num(s, prefix):
    v = s[len(prefix):] if s.startswith(prefix) else s
    try:
        return int(v)
    except ValueError:
        return None

counts = {"parsed": None, "compared": None, "drifted": None}
drift, absent = [], []
for line in lines("fields"):
    cells = line.split("\t")
    tag = cells[0]
    if tag == "COUNTS":
        for cell in cells[1:]:
            k, _, v = cell.partition("=")
            if k in counts:
                counts[k] = num(v, "")
    elif tag == "DRIFT" and len(cells) >= 9:
        drift.append({
            "kind": cells[1], "field": cells[2],
            "live_version": num(cells[3], "v"),
            "at": num(cells[4], "at="),
            "tree_len": num(cells[5], "tree="), "live_len": num(cells[6], "live="),
            "tree_window": cells[7], "live_window": cells[8],
        })
    elif tag == "ABSENT" and len(cells) >= 3:
        absent.append({"kind": cells[1], "field": cells[2]})

tenants = []
for line in lines("tenants"):
    cells = line.split("\t")
    if len(cells) >= 3:
        tenants.append({"file": cells[0], "not_admitted": num(cells[1], ""), "total": num(cells[2], "")})

report = {
    "at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "target": url,
    "verdict": verdict,
    "problems": problems,
    "live": {"admitted": len(lines("live"))},
    "authored": {"count": len(lines("authored"))},
    "exempt": lines("exempt"),
    "unauthored": lines("unauthored"),
    "fields": {**counts, "drift": drift, "absent": absent},
    "pending": lines("pending"),
    "tenants": tenants,
}
if len(drift) != (counts["drifted"] or 0):
    print(f"the report would carry {len(drift)} DRIFT line(s) against a drifted={counts['drifted']} count — the comparator's lines and its count disagree, so nothing was written", file=sys.stderr)
    sys.exit(1)
pathlib.Path(out).write_text(json.dumps(report, indent=2) + "\n")
PY
    rm -rf "$t"
}

# Fixtures, then the six refusals the comparator owes. Run on EVERY
# invocation, not behind a flag: on the forge host the live half below
# SKIPS, and without this the gate would exercise none of this code at
# all — a check that is not running (§Diagnosis). It is hermetic and
# costs one python per case.
self_test() {
    local t rc out
    t=$(mktemp -d) || return 1
    mkdir -p "$t/bundle"
    cat > "$t/bundle/alpha.toml" <<'FX'
[[workflow]]
kind = "alpha"
label = "Alpha"
category = "platform"
owning_team = "platform"
description = "The first protocol."
FX
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol."}]' > "$t/match.json"
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol, as described by a service deleted last week."}]' > "$t/drift.json"
    printf '%s' '[{"kind":"gamma","version":1,"status":"active","label":"Gamma","description":"Another world entirely."}]' > "$t/elsewhere.json"
    printf '%s' '[{"kind":"alpha","version":1,"status":"retired","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."}]' > "$t/retired.json"
    printf '%s' 'not json at all' > "$t/garbage.json"

    local FIELD_FLOOR_SAVED="$FIELD_FLOOR"
    FIELD_FLOOR=2

    # 1. Agreement is silence — and it still says how much it compared.
    out=$(fields_report "$t/bundle" "$t/match.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: matching fixtures exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "COUNTS	parsed=2	compared=2	drifted=0" <<< "$out" \
        || { echo "self-test FAILED: matching fixtures did not report 2 compared / 0 drifted: $out" >&2; rm -rf "$t"; return 1; }

    # 2. THE RED THIS CHECK EXISTS FOR: one description differs, and the
    #    finding must NAME the kind and the field.
    out=$(fields_report "$t/bundle" "$t/drift.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: a drifting description exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	description	v3" <<< "$out" \
        || { echo "self-test FAILED: the drift was not named by kind, field and live version: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=1" <<< "$out" \
        || { echo "self-test FAILED: the drift was not counted: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "deleted last week" <<< "$out" \
        || { echo "self-test FAILED: the finding carries no excerpt of the live text: $out" >&2; rm -rf "$t"; return 1; }

    # 3. A FIELD THE FILE DOES NOT CLAIM is named rather than quietly
    #    dropped — the same vacuity one field down.
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
FX
    out=$(fields_report "$t/bundle" "$t/drift.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: an absent claim exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "ABSENT	beta	description" <<< "$out" \
        || { echo "self-test FAILED: a file claiming no description was not named: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=0" <<< "$out" \
        || { echo "self-test FAILED: an absent claim was counted as drift: $out" >&2; rm -rf "$t"; return 1; }
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX

    # 3b. THE STRUCTURAL RED (2026-09-15, backlog 0ccf23ec). The live row
    #    carries a step and a required field the file lacks, and one
    #    step's label differs. The finding must name the step COUNT, the
    #    title LIST, and the one step whose required set and label
    #    differ — while a step that agrees stays silent and a step
    #    present on ONE side only is named by the title list, not by a
    #    per-step line against `<absent>`. This is the measurement that
    #    was missing when ship-a-change's live v31 read as "one
    #    description adrift" beside a file with eight steps to live's
    #    nine and no `proof` field: publishing the file over the row
    #    would have deleted both.
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."

[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
title_template = "Opened"

[[workflow.step]]
title = "proven"
kind = "task"
ready_when = "steps.opened.done"
title_template = "Proven"
[[workflow.step.fields]]
name = "verified"
field_type = "string"
required = true
FX
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol.",
                   "steps":[{"title":"opened","kind":"trigger","ready_when":"true","title_template":"Opened","fields":[]},
                            {"title":"proven","kind":"task","ready_when":"steps.opened.done","title_template":"Proven in prod","fields":[{"name":"verified","field_type":"string","required":true},{"name":"method","field_type":"string","required":false},{"name":"proof","field_type":"string","required":true}]},
                            {"title":"settled","kind":"outcome","ready_when":"steps.proven.done","title_template":"Settled","fields":[]}]}]' > "$t/steps.json"
    out=$(fields_report "$t/bundle" "$t/steps.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: a live row with an extra step exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	steps.count	v3" <<< "$out" \
        || { echo "self-test FAILED: a step the file lacks did not drift the step count: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	steps.titles	v3" <<< "$out" \
        || { echo "self-test FAILED: a step the file lacks was not named in the title list: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "opened,proven,settled" <<< "$out" \
        || { echo "self-test FAILED: the title-list finding carries no excerpt of the live titles: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	steps.proven.required	v3" <<< "$out" \
        || { echo "self-test FAILED: a required field the file lacks was not named by its step: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "proof,verified" <<< "$out" \
        || { echo "self-test FAILED: the required-set finding carries no excerpt of the live set: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	steps.proven.title_template	v3" <<< "$out" \
        || { echo "self-test FAILED: a step label that differs was not named by its step: $out" >&2; rm -rf "$t"; return 1; }
    # The live `method` is OPTIONAL and the file lacks it: named since
    # 2026-09-28 (case 3d says why), so this fixture drifts five facets.
    grep -qF "DRIFT	beta	steps.proven.optional	v3" <<< "$out" \
        || { echo "self-test FAILED: an optional field the file lacks was not named by its step: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=5" <<< "$out" \
        || { echo "self-test FAILED: five step facets adrift were not counted as five: $out" >&2; rm -rf "$t"; return 1; }
    grep -q "steps\.opened\." <<< "$out" \
        && { echo "self-test FAILED: a step that agrees was named: $out" >&2; rm -rf "$t"; return 1; }
    grep -q "steps\.settled\." <<< "$out" \
        && { echo "self-test FAILED: a step on one side only got a per-step line: $out" >&2; rm -rf "$t"; return 1; }
    grep -q "DRIFT	beta	description" <<< "$out" \
        && { echo "self-test FAILED: an agreeing description was named beside the step drift: $out" >&2; rm -rf "$t"; return 1; }
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX

    # 3c. THE AGENT BLOCK (2026-09-19, backlog 1b847556). The file says
    #    WHO runs `proven` — a profile, a model, a budget, an effort —
    #    and the live row does not. Measured 2026-09-18 23:25Z: the live
    #    backlog-item v2 carried no block on any step beside a file with
    #    two, and this comparator answered "54 live rows agree with
    #    their file", so a drift in who executes a step was invisible
    #    to the daily measurement. The finding must name the STEP; the
    #    same block on both sides must agree although the file spells
    #    the budget as the integer 5 and the registry hands back 5.0
    #    (a TOML integer becomes a JSON float through AgentSpec's f64).
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."

[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
title_template = "Opened"

[[workflow.step]]
title = "proven"
kind = "task"
ready_when = "steps.opened.done"
title_template = "Proven"
agent = { profile = "builder", model = "opus-5[1m]", budget_usd = 5, effort = "high" }
FX
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol.",
                   "steps":[{"title":"opened","kind":"trigger","ready_when":"true","title_template":"Opened","fields":[],"agent":null},
                            {"title":"proven","kind":"task","ready_when":"steps.opened.done","title_template":"Proven","fields":[],"agent":null}]}]' > "$t/agent-missing.json"
    out=$(fields_report "$t/bundle" "$t/agent-missing.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: a live row lacking an agent block exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "DRIFT	beta	steps.proven.agent	v3" <<< "$out" \
        || { echo "self-test FAILED: a live step lacking the agent block its file declares was not named by its step: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF '"budget_usd": 5.0, "effort": "high", "executor_provenance": "advisory"' <<< "$out" \
        || { echo "self-test FAILED: the agent finding carries no excerpt of the block the file declares: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=1" <<< "$out" \
        || { echo "self-test FAILED: one agent block adrift was not counted as one: $out" >&2; rm -rf "$t"; return 1; }
    grep -q "steps\.opened\.agent" <<< "$out" \
        && { echo "self-test FAILED: a step with no block on either side was named: $out" >&2; rm -rf "$t"; return 1; }
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol.",
                   "steps":[{"title":"opened","kind":"trigger","ready_when":"true","title_template":"Opened","fields":[],"agent":null},
                            {"title":"proven","kind":"task","ready_when":"steps.opened.done","title_template":"Proven","fields":[],"agent":{"profile":"builder","model":"opus-5[1m]","budget_usd":5.0,"effort":"high"}}]}]' > "$t/agent-equal.json"
    out=$(fields_report "$t/bundle" "$t/agent-equal.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: a live row carrying the file's agent block exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=0" <<< "$out" \
        || { echo "self-test FAILED: the same agent block on both sides (TOML 5, JSON 5.0) read as drift: $out" >&2; rm -rf "$t"; return 1; }
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX

    # 3d. A TREE REVISION THE REGISTRY NEVER TOOK, in the facets the list
    #    above used to leave out (2026-09-28, backlog 462cdfe3). The file
    #    adds an OPTIONAL field, rewrites a step's procedure, re-orders a
    #    predicate, and moves the protocol to another surface — none of
    #    which touches a required field, a title or a label, so every one
    #    read "agree". Measured that day: incident's file said
    #    `surfaces = ["system-incidents"]` and publish-request's
    #    `owner_role = "platform-admin"`, both landed by cars, and the live
    #    rows still said `system-design` and `shift-lead`. Each finding
    #    must be named by its key; what the PUBLISH PATH fills in when a
    #    file is silent (`authority_role` from the owner role, `false`,
    #    `null`, an empty list) must not be; and a number spelled 2 in
    #    TOML and 2.0 by the registry must agree.
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
metadata = { surfaces = ["system-incidents"] }

[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
title_template = "Opened"
duration_hours = 2

[[workflow.step]]
title = "revoke"
kind = "task"
ready_when = "steps.opened.done && steps.opened.done"
title_template = "Revoke"
metadata_defaults = { procedure = "Revoke the old token at the issuer." }
[[workflow.step.fields]]
name = "revoked"
field_type = "string"
required = true
[[workflow.step.fields]]
name = "old_token"
field_type = "string"
FX
    printf '%s' '[{"kind":"alpha","version":1,"status":"active","label":"Alpha","category":"platform","owning_team":"platform","description":"The first protocol."},
                  {"kind":"beta","version":3,"status":"active","label":"Beta","category":"platform","owning_team":"platform","description":"The second protocol.","metadata":{"surfaces":["system-design"]},"metadata_schema":null,"entitlements":[],
                   "steps":[{"title":"opened","kind":"trigger","ready_when":"true","title_template":"Opened","fields":[],"agent":null,"duration_hours":2.0,"authority_role":"platform-admin","claimable":false,"terminal":null},
                            {"title":"revoke","kind":"task","ready_when":"steps.opened.done","title_template":"Revoke","fields":[{"name":"revoked","field_type":"string","required":true}],"agent":null,"authority_role":"platform-admin","metadata_defaults":{"procedure":"Revoke it."}}]}]' > "$t/unpublished.json"
    out=$(fields_report "$t/bundle" "$t/unpublished.json" 2>&1); rc=$?
    [ "$rc" -eq 0 ] || { echo "self-test FAILED: an unpublished tree revision exited $rc: $out" >&2; rm -rf "$t"; return 1; }
    for want in "DRIFT	beta	metadata	v3" "DRIFT	beta	steps.revoke.optional	v3" \
                "DRIFT	beta	steps.revoke.metadata_defaults	v3" "DRIFT	beta	steps.revoke.ready_when	v3"; do
        grep -qF "$want" <<< "$out" \
            || { echo "self-test FAILED: an unpublished tree revision was not named as '$want': $out" >&2; rm -rf "$t"; return 1; }
    done
    grep -qF "system-incidents" <<< "$out" \
        || { echo "self-test FAILED: the metadata finding carries no excerpt of the file's value: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "drifted=4" <<< "$out" \
        || { echo "self-test FAILED: four unpublished facets were not counted as four: $out" >&2; rm -rf "$t"; return 1; }
    grep -qE "authority_role|claimable|terminal|metadata_schema|entitlements|duration_hours" <<< "$out" \
        && { echo "self-test FAILED: a value the publish path fills in, or a number spelled 2 and 2.0, was named as drift: $out" >&2; rm -rf "$t"; return 1; }
    cat > "$t/bundle/beta.toml" <<'FX'
[[workflow]]
kind = "beta"
label = "Beta"
category = "platform"
owning_team = "platform"
description = "The second protocol."
FX

    # 4. THE FLOOR. A registry answering about kinds this bundle does
    #    not hold finds no drift, which must never read as clean.
    out=$(fields_report "$t/bundle" "$t/elsewhere.json" 2>&1); rc=$?
    [ "$rc" -eq 7 ] || { echo "self-test FAILED: a zero-kind comparison exited $rc, expected 7: $out" >&2; rm -rf "$t"; return 1; }
    grep -qF "REFUSED" <<< "$out" \
        || { echo "self-test FAILED: the floor refusal does not say so: $out" >&2; rm -rf "$t"; return 1; }

    # 5. A retired row is not an active one — it must not stand in for
    #    the comparison, and here that empties it below the floor.
    out=$(fields_report "$t/bundle" "$t/retired.json" 2>&1); rc=$?
    [ "$rc" -eq 7 ] || { echo "self-test FAILED: a retired-only row exited $rc, expected the floor's 7: $out" >&2; rm -rf "$t"; return 1; }

    # 6. An answer that is not JSON is no answer.
    out=$(fields_report "$t/bundle" "$t/garbage.json" 2>&1); rc=$?
    [ "$rc" -eq 3 ] || { echo "self-test FAILED: an unparseable live answer exited $rc, expected 3: $out" >&2; rm -rf "$t"; return 1; }

    # 7. A bundle file the reader cannot read is a broken reader, not an
    #    agreeing protocol.
    printf '%s' 'kind = "gamma"' > "$t/bundle/gamma.toml"
    out=$(fields_report "$t/bundle" "$t/match.json" 2>&1); rc=$?
    [ "$rc" -eq 6 ] || { echo "self-test FAILED: a file with no [[workflow]] exited $rc, expected 6: $out" >&2; rm -rf "$t"; return 1; }
    rm -f "$t/bundle/gamma.toml"

    FIELD_FLOOR="$FIELD_FLOOR_SAVED"

    # 9. THE JSON REPORT carries the comparator's facts, not a retelling
    #    of them: driven from the SAME drift fixture as case 2, in a
    #    subshell that stands in for the live half's variables, it must
    #    name the same kind, field, live version and excerpt, count the
    #    same drift, and carry the verdict it was handed.
    out=$(fields_report "$t/bundle" "$t/drift.json" 2>/dev/null)
    (
        live_kinds=$'alpha\nbeta\ngamma'; authored=$'alpha\nbeta\ndelta'; unauthored="gamma"
        fields_out="$out"; pending="delta"; tenant_lines=$'examples/x/seeds/workflows.toml\t3\t25'
        problems=1; EXEMPT=()
        write_report "$t/report.json" 2
    ) || { echo "self-test FAILED: write_report refused the drift fixture" >&2; rm -rf "$t"; return 1; }
    out=$(python3 - "$t/report.json" <<'PY' 2>&1
import json, sys
r = json.load(open(sys.argv[1]))
want = {
    "live.admitted": (r["live"]["admitted"], 3), "authored.count": (r["authored"]["count"], 3),
    "unauthored": (r["unauthored"], ["gamma"]), "pending": (r["pending"], ["delta"]),
    "exempt": (r["exempt"], []), "verdict": (r["verdict"], 2), "problems": (r["problems"], 1),
    "fields.parsed": (r["fields"]["parsed"], 2), "fields.compared": (r["fields"]["compared"], 2),
    "fields.drifted": (r["fields"]["drifted"], 1), "len(fields.drift)": (len(r["fields"]["drift"]), 1),
    "drift.kind": (r["fields"]["drift"][0]["kind"], "beta"),
    "drift.field": (r["fields"]["drift"][0]["field"], "description"),
    "drift.live_version": (r["fields"]["drift"][0]["live_version"], 3),
    "tenants": (r["tenants"], [{"file": "examples/x/seeds/workflows.toml", "not_admitted": 3, "total": 25}]),
}
bad = [f"{k}: got {g!r}, want {w!r}" for k, (g, w) in want.items() if g != w]
if "deleted last week" not in r["fields"]["drift"][0]["live_window"]:
    bad.append("drift.live_window carries no excerpt of the live text")
if not r["at"].endswith("Z") or r["target"] == "":
    bad.append("at/target missing")
print("\n".join(bad)); sys.exit(1 if bad else 0)
PY
    ) || { echo "self-test FAILED: the JSON report does not carry the text report's facts: $out" >&2; rm -rf "$t"; return 1; }

    # 8. THE UNREACHABLE CASE, end to end, because it is the one that
    #    must never read as success. `--require-live` is the mode for a
    #    caller with somewhere to put the answer, and it must exit 75
    #    (EX_TEMPFAIL) rather than 0 when the registry cannot be read.
    #    A link-local port nothing listens on: refused in ~6ms, and no
    #    DNS lookup, so the resolver flake cannot make this hang. A
    #    refused connect is waited out as a rollout since backlog
    #    834ddb7c (lib/sor-read.sh), so this child — which runs on every
    #    invocation, the gate's included — asks for no wait at all:
    #    without the zero it would sit a full window on a port that is
    #    refused by design.
    if [ -z "${BOSS_LINT_SELFTEST_CHILD:-}" ]; then
        out=$(BOSS_LINT_SELFTEST_CHILD=1 BOSS_SOR_WAIT_SECONDS=0 BOSS_JOBS_URL="http://[::1]:9" bash "$0" --require-live 2>&1); rc=$?
        [ "$rc" -eq 75 ] || { echo "self-test FAILED: --require-live against an unreachable registry exited $rc, expected 75: $out" >&2; rm -rf "$t"; return 1; }
        grep -qF "SKIPPED the live comparison" <<< "$out" \
            || { echo "self-test FAILED: the skip is not loud: $out" >&2; rm -rf "$t"; return 1; }
        grep -qF "[::1]:9" <<< "$out" \
            || { echo "self-test FAILED: the skip does not name what it could not reach: $out" >&2; rm -rf "$t"; return 1; }
        grep -qi "OK —" <<< "$out" \
            && { echo "self-test FAILED: a skip printed an OK line: $out" >&2; rm -rf "$t"; return 1; }
    fi

    rm -rf "$t"
    [ "$SELF_TEST" -eq 1 ] && echo "$NAME: self-test ok — agreement is silent and counted, a drifting description is named by kind/field/version with an excerpt, a field the file does not claim is named and not counted as drift, a step the file lacks is named by count and title list and a required field or step label that differs is named by its step, a live step lacking the agent block its file declares is named by its step while the same block on both sides agrees with the budget as 5 and 5.0, a tree revision the registry never took (an optional field, a procedure, a predicate, the workflow metadata) is named by its key while a value the publish path fills in when the file is silent is not, a comparison below the floor and a retired-only row are refused, an unparseable answer and an unreadable bundle file each refuse distinctly, and --require-live exits 75 naming the target it could not reach, and the JSON report carries the same kind/field/version/excerpt, counts and verdict as the text"
    return 0
}

if [ -n "$COMPARE_BUNDLE" ]; then
    fields_report "$COMPARE_BUNDLE" "$COMPARE_LIVE"
    exit $?
fi

self_test || exit 1
[ "$SELF_TEST" -eq 0 ] || exit 0

[ -d "$BUNDLE" ] || { echo "the-live-protocols-are-the-authored-protocols: $BUNDLE does not exist" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Static half — always runs, needs no network.
# ---------------------------------------------------------------------------
shopt -s nullglob
bundle_files=("$BUNDLE"/*.toml)
tenant_files=($TENANT_GLOB)
shopt -u nullglob
[ ${#bundle_files[@]} -gt 0 ] || { echo "the-live-protocols-are-the-authored-protocols: no *.toml in $BUNDLE — a wrong path, not an empty bundle" >&2; exit 1; }
[ ${#tenant_files[@]} -gt 0 ] || { echo "the-live-protocols-are-the-authored-protocols: no tenant seed matched $TENANT_GLOB — a wrong path, not a tenantless tree" >&2; exit 1; }
[ -f "$REGISTRY_RS" ] || { echo "the-live-protocols-are-the-authored-protocols: $REGISTRY_RS does not exist" >&2; exit 1; }

# Home 1: the bundle listing. One kind per file, named for the kind.
bundle_kinds=$(for f in "${bundle_files[@]}"; do basename "$f" .toml; done | LC_ALL=C sort -u)

# Home 2: a tenant bundle's `[[workflow]]` blocks. The kind key is taken
# ONLY inside a `[[workflow]]` table, never inside `[[workflow.step]]` —
# a step's `kind` is a StepType (`task`, `trigger`, `outcome`), and
# sweeping those in is what makes a naive grep's authored set too LOOSE.
# Loose under-reports the gap instead of over-reporting it, which is the
# quieter failure and so the one worth spelling out.
tenant_kinds_of() {
    LC_ALL=C awk '
        /^[ \t]*\[\[[ \t]*workflow[ \t]*\]\]/ { in_wf = 1; next }
        /^[ \t]*\[/                           { in_wf = 0 }
        in_wf && /^[ \t]*kind[ \t]*=/ {
            if (match($0, /"[^"]+"/)) print substr($0, RSTART + 1, RLENGTH - 2)
        }
    ' "$1"
}
tenant_kinds=$(for f in "${tenant_files[@]}"; do tenant_kinds_of "$f"; done | LC_ALL=C sort -u)

# Home 3: the Rust literals inside `platform_workflows()`. EMPTY since
# 2026-09-11, and the check below is what keeps it that way.
#
# This home was always a tolerated waypoint, never a destination: a kind
# here is a protocol that cannot be changed without building and
# shipping a binary, which is CLAUDE.md's own definition of a protocol
# that has leaked into the substrate. The last four left on 2026-09-11
# (`design-doc-review` and the three `maintenance_spec` chores), and the
# concrete cost of the form is on the record — `maintenance_spec` put its
# description in the CATEGORY column for all three, and because
# `bootstrap_reconcile` re-asserts a code spec on every boot the wrong
# value could not drift back on its own and every new deployment
# reproduced it (6c796f75).
#
# So the roster is now checked EMPTY rather than merely scraped. The
# body is read between the signature and the first line closing it at
# column 0; two entry shapes are recognised — a kebab-case string
# literal (`maintenance_spec("maintenance-backup", …)`) and a
# no-argument `<name>_spec()` call, whose kind is its name with
# underscores as dashes (`design_doc_review_spec()` →
# `design-doc-review`) — and they still feed the authored set, so a
# literal put back tomorrow is counted as authoring and NOT reported as
# an unauthored live kind. It fails on the narrower ground that a
# protocol belongs in the bundle.
#
# An empty roster must also LOOK empty, because "the scrape saw nothing"
# and "there is nothing to see" have to stay distinguishable: the body
# with comments and whitespace stripped is required to be exactly
# `vec![]`. That is what keeps this honest against an entry shape the
# two patterns above do not recognise — such an entry leaves the body
# non-empty, and a non-empty body is a failure whether or not a kind
# name could be read out of it.
code_kinds=$(
    LC_ALL=C awk '
        /^pub fn platform_workflows\(\)/ { inside = 1; next }
        inside && /^\}/                  { inside = 0 }
        inside {
            line = $0
            sub(/\/\/.*$/, "", line)
            s = line
            while (match(s, /"[a-z][a-z0-9-]*"/)) {
                print substr(s, RSTART + 1, RLENGTH - 2)
                s = substr(s, RSTART + RLENGTH)
            }
            s = line
            while (match(s, /[a-z][a-z0-9_]*_spec\(\)/)) {
                name = substr(s, RSTART, RLENGTH - 2)
                sub(/_spec$/, "", name)
                gsub(/_/, "-", name)
                print name
                s = substr(s, RSTART + RLENGTH)
            }
        }
    ' "$REGISTRY_RS" | LC_ALL=C sort -u
)

# The roster's body, comments and whitespace gone. `absent` means the
# signature itself could not be found — a renamed function, which is a
# broken scrape and not an empty roster.
roster_body=$(
    LC_ALL=C awk '
        /^pub fn platform_workflows\(\)/ { inside = 1; found = 1; next }
        inside && /^\}/                  { inside = 0 }
        inside {
            line = $0
            sub(/\/\/.*$/, "", line)
            gsub(/[ \t]/, "", line)
            body = body line
        }
        END { if (!found) print "absent"; else print body }
    ' "$REGISTRY_RS"
)
if [ "$roster_body" = "absent" ]; then
    fail "could not find \`pub fn platform_workflows()\` in $REGISTRY_RS — the scrape \
broke, so a green result would mean nothing"
elif [ "$roster_body" != "vec![]" ]; then
    n=$(printf '%s\n' "$code_kinds" | LC_ALL=C sed '/^$/d' | wc -l | tr -d ' ')
    fail "platform_workflows() authors $n workflow kind(s) as Rust literals:"
    printf '    %s\n' ${code_kinds:+$code_kinds} >&2
    echo "" >&2
    echo "  A protocol here cannot be changed without building and shipping a" >&2
    echo "  binary — CLAUDE.md's own definition of a protocol that has leaked" >&2
    echo "  into the substrate — and bootstrap_reconcile RE-ASSERTS it on every" >&2
    echo "  boot, so an operator's edit to the live row is reverted at the next" >&2
    echo "  pod roll and a wrong value cannot drift back (6c796f75)." >&2
    echo "" >&2
    echo "  Move it to $BUNDLE/<kind>.toml, rendered from the live row, and" >&2
    echo "  delete the literal. The row the deployment already has is NOT" >&2
    echo "  touched: the seed is insert-if-missing, so the kind keeps its" >&2
    echo "  current version and every in-flight packet keeps its spec — it" >&2
    echo "  simply stops being reconciled. See $BUNDLE/README.md." >&2
    echo "" >&2
    echo "  The roster must also READ as empty — \`vec![]\` with nothing but" >&2
    echo "  comments — so that an entry shape this script does not recognise" >&2
    echo "  still fails here instead of passing silently." >&2
fi

# Home 4: a migration that inserts a row directly. Only files that
# actually write a `workflows` row are read, and every single-quoted
# kebab-case literal in such a file counts. That is deliberately
# generous — `'active'` is swept up too — because a spurious authored
# name can only under-report the gap, while a missed real one would red
# a car for a protocol the tree does describe.
#
# The pattern is an ERE rather than the literal SQL phrase so that
# `api-path-bypass-smell` does not read this lint's own SQL-reading
# grep as a lint that writes to the database.
migration_kinds=$(
    LC_ALL=C grep -lE "INSERT[[:space:]]+INTO[[:space:]]+workflows" "$SCHEMA_DIR"/*.sql 2>/dev/null \
        | while IFS= read -r f; do
            LC_ALL=C grep -oE "'[a-z][a-z0-9-]*'" "$f" | tr -d "'"
        done | LC_ALL=C sort -u
)

authored=$(printf '%s\n%s\n%s\n%s\n' \
    "$bundle_kinds" "$tenant_kinds" "$code_kinds" "$migration_kinds" \
    | LC_ALL=C sed '/^$/d' | LC_ALL=C sort -u)

# An exemption for a kind the tree now authors is refused. Left standing
# it would keep a future live-authored kind of that name out of this
# check without anyone deciding so — the same reason gate.sh reads its
# pre-flight exclusions off the excluded lints' own headers, where a
# lint that no longer exists cannot be named.
for kind in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    if LC_ALL=C grep -qxF "$kind" <<<"$authored"; then
        fail "the exemption for \`$kind\` is stale — the tree now authors it"
        echo "" >&2
        echo "  Drop \`$kind\` from EXEMPT in this script. An exemption that" >&2
        echo "  outlives its reason silently excuses the next live-authored" >&2
        echo "  protocol that happens to share the name." >&2
    fi
done

# ---------------------------------------------------------------------------
# Live half — skips loudly when the read surface is unreachable.
# ---------------------------------------------------------------------------
skip() {
    echo "$NAME: $LINT_CANNOT_ANSWER_MARKER — SKIPPED the live comparison — $1" >&2
    echo "  target: $URL (override with BOSS_JOBS_URL)" >&2
    echo "  The exemption set was still checked against the tree" >&2
    echo "  (${#EXEMPT[@]} exemptions, $(printf '%s\n' "$authored" | wc -l | tr -d ' ') authored kinds)." >&2
    echo "  NOTHING IS CLAIMED about what the running registry admits, or about" >&2
    echo "  whether any live row still says what its file says — the field" >&2
    echo "  comparison did not run, so ZERO kinds were compared." >&2
    # A report is the comparison, rendered; with no comparison there is
    # nothing to render, and a file saying "0 drifted" would be read as
    # a clean bill by the caller that asked for it (infra/protocol-drift.sh
    # refuses on exactly this line).
    [ -z "$REPORT_JSON" ] || echo "  no report was written to $REPORT_JSON — a report of no comparison would read as no drift." >&2
    [ "$problems" -eq 0 ] || exit 1
    # A caller with somewhere to put the answer runs `--require-live`,
    # and for it "I could not read the registry" must not be the same
    # exit as "I read it and it agrees". 75 is EX_TEMPFAIL, the code
    # this tree already uses for a run that could not happen rather
    # than one that failed (infra/gate-runner/run.sh, checkout-lock.sh),
    # and the one infra/protocol-drift.sh reads by number.
    [ "$REQUIRE_LIVE" -eq 0 ] || exit 75
    # The bare invocation is the gate's, and for it the answer is the
    # lint vocabulary: 3, "the machine could not answer". No scanned
    # line is printed on this path — the comparison did not run, so a
    # count would certify it — and gate.sh turns this exit into a
    # refusal receipt instead of a red (backlog a26f92c4).
    exit "$LINT_CANNOT_ANSWER"
}

# A run with no estate at all (the public mirror) says so rather than
# being refused on every run for a route it will never have (backlog
# 3e63662c; lib/no-estate.sh). Only the bare invocation consults it:
# `--require-live` asks for the comparison, and for that caller "could
# not compare" stays 75. The tree half above has already run and
# recorded its findings, and a finding is still this lint's red.
if [ "$REQUIRE_LIVE" -eq 0 ] \
    && lint_no_estate "$NAME" "the live workflow registry ($URL)"; then
    [ -z "$REPORT_JSON" ] || echo "  no report was written to $REPORT_JSON — a report of no comparison would read as no drift." >&2
    [ "$problems" -eq 0 ] || exit 1
    lint_scanned "$NAME" "$(printf '%s\n' "$authored" | LC_ALL=C sed '/^$/d' | wc -l | tr -d ' ')" \
        "authored kind(s) — the tree half only; the live comparison was NOT made"
    exit 0
fi

command -v curl >/dev/null 2>&1 || skip "curl is not on this box"
command -v python3 >/dev/null 2>&1 || skip "python3 is not on this box"

body=$(mktemp) || exit 1
trap 'rm -f "$body"' EXIT
code=$(lint_sor_read "$NAME" "the jobs API" "$URL" "$body")
# 000 is curl's "never got an answer" — no route, refused, timed out.
[ "$code" = "200" ] || skip "$URL answered HTTP $code"

# One admitted kind per line. The surface answers a top-level ARRAY of
# registry rows; anything else is a different endpoint, or an error body
# wearing a 200. Exit 3 and 4 separate those two cases so the message
# can say which.
live_kinds=$(python3 - "$body" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"unparseable: {e}", file=sys.stderr)
    sys.exit(3)
rows = doc.get("workflows") if isinstance(doc, dict) else doc
if not isinstance(rows, list) or not all(isinstance(r, dict) for r in rows):
    shape = sorted(doc) if isinstance(doc, dict) else type(doc).__name__
    print(f"not an array of workflow rows (got {shape})", file=sys.stderr)
    sys.exit(4)
kinds = sorted({r["kind"] for r in rows if r.get("status", "active") == "active" and "kind" in r})
if not kinds:
    sys.exit(5)
print("\n".join(kinds))
PY
)
case "$?" in
    0) ;;
    # A 200 that is not JSON is a proxy or a captive portal answering for
    # something that never reached the registry — no answer, wearing a
    # 200. The python error above is the evidence; treat it as no route.
    3) skip "the response did not parse as JSON — treated as no answer" ;;
    4) fail "$URL answered 200 with no array of workflow rows — a 200 from the \
wrong surface, or an error body; either way nothing read the registry"
       exit 1 ;;
    5) fail "$URL reports ZERO admitted workflow kinds"
       echo "" >&2
       echo "  An empty registry is not a clean comparison. A deployment that" >&2
       echo "  admits no kinds can open no Job of any sort: no gate run, no" >&2
       echo "  change shipped, no chore recorded. Read as 'nothing to compare'" >&2
       echo "  this would be the confident wrong answer." >&2
       exit 1 ;;
    *) skip "could not read kinds from the response" ;;
esac

# A DELIVERED TENANT'S PROTOCOLS ARE AUTHORED — IN THAT TENANT'S REPO.
# Since 2026-09-16 (the prod flip, f4f5c387 + ee7b62bb) an instance may
# take its tenant from a repo the converge checks out (instances.toml
# `tenant_repo`), and `boss tenant publish` admits that tenant's
# workflows.toml with `owning_team = <its tenant_id>`. Those rows have a
# file, a diff and a second reader — in the tenant's own repository,
# which this tree does not carry and must not (business specifics never
# enter the product repo: fcc1d57b). Measured 2026-09-17 00:0xZ, the
# first pre-flight after the flip: `receive-a-sponsorship`
# (owning_team algedonic) read UNAUTHORED and would have reddened every
# gate. So when the tree declares at least one repo-sourced instance, a
# live kind whose owning_team names no team this tree authors for is
# read as that tenant's, not as unauthored; it is listed, never
# silently passed. A tree with no repo-sourced instance keeps the old
# reading in full.
repo_tenants=$(awk '/^[[:space:]]*#/ { next } $1 == "tenant_repo" { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); print }' infra/cluster/instances.toml 2>/dev/null | LC_ALL=C sort -u)
delivered_tenant_kinds=""
if [ -n "$repo_tenants" ]; then
    delivered_tenant_kinds=$(python3 - "$body" <(printf '%s\n' "$authored") <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
rows = doc.get("workflows") if isinstance(doc, dict) else doc
authored = set(l.strip() for l in open(sys.argv[2]) if l.strip())
rows = [r for r in rows if r.get("status", "active") == "active" and "kind" in r]
tree_teams = {r.get("owning_team") for r in rows if r["kind"] in authored}
print("\n".join(sorted(r["kind"] for r in rows if r["kind"] not in authored and r.get("owning_team") and r.get("owning_team") not in tree_teams)))
PY
)
fi

unauthored=$(
    LC_ALL=C comm -23 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$authored") \
        | { if [ ${#EXEMPT[@]} -gt 0 ]; then LC_ALL=C grep -vxF -f <(printf '%s\n' "${EXEMPT[@]}"); else cat; fi; } \
        | { if [ -n "$delivered_tenant_kinds" ]; then LC_ALL=C grep -vxF -f <(printf '%s\n' "$delivered_tenant_kinds"); else cat; fi; } \
        | LC_ALL=C sed '/^$/d'
)
if [ -n "$delivered_tenant_kinds" ]; then
    n=$(printf '%s\n' "$delivered_tenant_kinds" | wc -l | tr -d ' ')
    echo "$NAME: $n kind(s) authored by a delivered tenant (instances.toml tenant_repo: $(printf '%s' "$repo_tenants" | tr '\n' ' ')), read from the tenant's own repo, not this tree: $(printf '%s' "$delivered_tenant_kinds" | tr '\n' ' ')"
fi

# A stale exemption in the other direction: named here, not admitted
# live. Same reason as the authored check above — it would excuse a
# future kind of that name that nobody decided to excuse.
for kind in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    if ! LC_ALL=C grep -qxF "$kind" <<<"$live_kinds"; then
        fail "the exemption for \`$kind\` is stale — the live registry does not admit it"
        echo "" >&2
        echo "  Drop \`$kind\` from EXEMPT in this script." >&2
    fi
done

if [ -n "$unauthored" ]; then
    n=$(printf '%s\n' "$unauthored" | wc -l | tr -d ' ')
    fail "the registry admits $n workflow kind(s) this tree does not author:"
    printf '    %s\n' $unauthored >&2
    echo "" >&2
    echo "  Each of these reaches the registry without a file, so: a change to" >&2
    echo "  it is a live publish with no diff and no second reader, a fresh" >&2
    echo "  database will not have it at all, and answering \"what does this" >&2
    echo "  protocol require\" means querying production. Write it down:" >&2
    echo "" >&2
    echo "    $BUNDLE/<kind>.toml — one [[workflow]] named for the file." >&2
    echo "    Read the live body from GET $URL and render it; author it, do" >&2
    echo "    not retype it, because a file that DISAGREES with the live row" >&2
    echo "    replaces one problem with a worse one. ship-a-change.toml is the" >&2
    echo "    worked example and was generated for exactly that reason." >&2
    echo "" >&2
    echo "  A TENANT protocol (one only an example tenant such as the brewery" >&2
    echo "  runs) goes in that tenant's examples/<tenant>/seeds/workflows.toml" >&2
    echo "  instead, not in the platform bundle." >&2
    echo "" >&2
    echo "  NO MIGRATION. The bundle is applied by boss-platform-workflow-seed" >&2
    echo "  (insert-if-missing, run from infra/postgres/bootstrap-db.sh), so a" >&2
    echo "  fresh database gets the file and an existing row is untouched. An" >&2
    echo "  insert-into-workflows migration would be a SECOND copy of the same" >&2
    echo "  fact (§9a) and would skip the viability lint that publish runs." >&2
    echo "" >&2
    echo "  See $BUNDLE/README.md. Exempting it in this script instead is a" >&2
    echo "  decision, and it belongs in the diff a reviewer reads." >&2
fi

# ---------------------------------------------------------------------------
# Field half — a live row still says what its FILE says.
# ---------------------------------------------------------------------------
# The half above asks whether the tree can DESCRIBE every live protocol.
# This one asks whether the description is still TRUE, which is a
# different question with its own measured instance: design-doc-review's
# live v1 `description` named `boss-docs-api`, `/api/design/pending-
# decisions` and the flush jobs — every one deleted on 2026-09-10 — while
# the tree file carried the corrected text, and TWO mechanisms each
# declined to notice. The bundle seed is insert-if-missing, so a present
# row is skipped whole. `bootstrap_reconcile`'s `kind_body_matches`
# excludes `description` as cosmetic, so it saw no drift and
# republished nothing (backlog e882b74c).
#
# NOT A CASE FOR WIDENING `kind_body_matches`, and the reason is
# stronger than the churn argument that excluded the field: since
# 2026-09-11 `platform_workflows()` is EMPTY, so reconcile iterates
# nothing and touches none of these rows. Widening its comparison
# today would change behaviour only for rows this problem is not
# about, and would still compare nothing here. It is also worth being
# exact about what that function already covers — `label`, `category`
# and `owning_team` ARE in it, and `description` alone is not — which
# is why all three are compared here: for a bundle-authored kind,
# nothing compares any of them.
#
# REPORTED, NEVER FAILED ON, in the bare invocation. The direction is
# the same legitimate window the half above tolerates, read one field
# deeper: a car edits a description in the tree, merges, and the live
# row does not change until an operator publishes a new version. Failing
# would red every car from that merge until the publish — the churn
# argument that excluded the field from reconcile, re-arriving as a red
# gate. `--require-live` is the mode for a reader that can act: there a
# drift exits 2.
fields_out=""
fields_rc=0
if [ -n "$live_kinds" ]; then
    fields_out=$(fields_report "$BUNDLE" "$body" 2>&1)
    fields_rc=$?
fi
case "$fields_rc" in
    0) ;;
    3) fail "the live answer could not be read for the field comparison: \
$(head -1 <<<"$fields_out")"
       echo "" >&2
       echo "  The kind comparison above read this same body, so this is a shape" >&2
       echo "  change, not an unreachable registry. Nothing was compared." >&2 ;;
    6) fail "a bundle file under $BUNDLE could not be read: \
$(head -1 <<<"$fields_out")"
       echo "" >&2
       echo "  One [[workflow]] per file, named for the kind — the loader pins" >&2
       echo "  it (platform_bundle.rs) and this reader needs it too." >&2 ;;
    7) fail "the field comparison refused rather than report a vacuous clean bill:"
       printf '    %s\n' "$(printf '%s' "$fields_out" | sed -n 's/^REFUSED\t//p')" >&2
       echo "" >&2
       echo "  A comparison of nothing finds no drift, which reads exactly like" >&2
       echo "  agreement. Either the bundle reader broke or the registry is" >&2
       echo "  answering about a different world; in both cases a green here" >&2
       echo "  would be the confident wrong answer (CLAUDE.md §Doors)." >&2 ;;
    *) fail "the field comparison exited $fields_rc: $(head -1 <<<"$fields_out")" ;;
esac

drift_lines=$(printf '%s\n' "$fields_out" | LC_ALL=C sed -n 's/^DRIFT\t//p')
fields_compared=$(LC_ALL=C sed -n 's/.*\tcompared=\([0-9]*\).*/\1/;T;p;q' <<<"$fields_out")
fields_compared=${fields_compared:-0}
drift_n=0
if [ -n "$drift_lines" ]; then
    drift_n=$(printf '%s\n' "$drift_lines" | wc -l | tr -d ' ')
    echo "$NAME: $drift_n field(s) or step facet(s) where the live row disagrees with its file:" >&2
    printf '%s\n' "$drift_lines" | while IFS=$'\t' read -r kind field ver at tlen llen twin lwin; do
        echo "    $kind.$field — live $ver, first differs $at ($tlen vs $llen chars)" >&2
        echo "      file: $twin" >&2
        echo "      live: $lwin" >&2
    done
    echo "" >&2
    # WHICH OF THEM MAY BE PUBLISHED, read before the command below is
    # named (backlog c6bd9f18). A kind can be HELD out of every unattended
    # publish — a row that turns on a refusal goes live at a deliberate
    # publish (infra/platform/workflow-holds/README.md, backlog 083d240e)
    # — and for a held kind the tree is ahead of live BY DESIGN. The
    # command this report names is the one door a hold does not bind, and
    # it used to be printed for every drifting kind, held or not (review
    # 72485f08 of car ca5d0218, finding F1). The holds come from their one
    # reader (§9a). A kind that is not held reads exactly as it did, and
    # holds that cannot be read are not absent holds: then NO publish is
    # advised at all, and the reason is said.
    drift_kinds=$(printf '%s\n' "$drift_lines" | cut -f1 | LC_ALL=C sort -u)
    holds_rc=0
    if [ -f "$HOLDS_PY" ]; then
        holds_out=$(python3 "$HOLDS_PY" . 2>&1) || holds_rc=$?
    else
        holds_rc=127
        holds_out="this tree does not carry $HOLDS_PY"
    fi
    # A line the reader answers that is neither `held` nor `released` with
    # its five cells is an answer this report does not know how to read.
    if [ "$holds_rc" -eq 0 ] && [ -n "$holds_out" ]; then
        strange=$(printf '%s\n' "$holds_out" | awk -F'\t' '!((NF == 5) && ($1 == "held" || $1 == "released"))')
        [ -z "$strange" ] || { holds_rc=65; holds_out="$HOLDS_PY answered a line this report does not read: $strange"; }
    fi
    held_drift=""
    open_drift="$drift_kinds"
    if [ "$holds_rc" -eq 0 ]; then
        held_drift=$(printf '%s\n' "$holds_out" | awk -F'\t' '$1 == "held" { print $2 }' | LC_ALL=C sort -u \
            | LC_ALL=C comm -12 - <(printf '%s\n' "$drift_kinds"))
        open_drift=$(LC_ALL=C comm -23 <(printf '%s\n' "$drift_kinds") <(printf '%s\n' "$held_drift") | LC_ALL=C sed '/^$/d')
    fi
    if [ "$holds_rc" -ne 0 ]; then
        echo "  NO PUBLISH IS ADVISED HERE: the workflow holds could not be read" >&2
        echo "  (exit $holds_rc), so whether a kind above is held out of the publish" >&2
        echo "  cannot be told — and a hold that cannot be read is not a hold that" >&2
        echo "  is absent. The reader said:" >&2
        echo "" >&2
        [ -n "$holds_out" ] || holds_out="$HOLDS_PY exited $holds_rc and said nothing"
        printf '%s\n' "$holds_out" | sed 's/^problem\t//; s/^/    /' >&2
        echo "" >&2
        echo "  Repair $HOLDS_REL (or $HOLDS_PY) in a car and run this" >&2
        echo "  again; what clears each kind is worded only once the holds read whole." >&2
    elif [ -n "$open_drift" ]; then
    echo "  A description is what an operator reads to know what a protocol is" >&2
    echo "  for, so a stale one sends somebody looking for a surface that may" >&2
    echo "  not exist — which is exactly what design-doc-review's live v1 did" >&2
    echo "  (backlog e882b74c). Clear one by publishing a new version FROM the" >&2
    echo "  file, which is the only write that moves a live row:" >&2
    echo "" >&2
    echo "    boss workflow publish <kind> $BUNDLE/<kind>.toml" >&2
    echo "" >&2
    [ -z "$held_drift" ] || {
        echo "  That command is for the kind(s) that are NOT held: $(printf '%s\n' $open_drift | tr '\n' ' ')" >&2
        echo "" >&2
    }
    echo "  In-flight packets are safe: publish adds a version, and every open" >&2
    echo "  Job stays pinned to the one it was admitted under." >&2
    echo "" >&2
    echo "  If the FILE is the wrong copy, fix the file — but do not retype a" >&2
    echo "  sentence you know to be false to make this quiet. The tree carrying" >&2
    echo "  the corrected text while the row lags is the safe direction, and it" >&2
    echo "  is the state this check exists to make visible rather than to" >&2
    echo "  forbid." >&2
    echo "" >&2
    echo "  A steps.* facet adrift says the two copies disagree about what the" >&2
    echo "  protocol REQUIRES — a step one side lacks, a required field, a step" >&2
    echo "  label. Decide which copy is the record BEFORE publishing: when the" >&2
    echo "  live row is ahead (a version published live and never written" >&2
    echo "  back), publishing the file over it deletes what the row gained — on" >&2
    echo "  2026-09-15 that would have been ship-a-change's settled step and" >&2
    echo "  the proof field that makes proven a machine-run fact (0ccf23ec)." >&2
    echo "  FOLD the row into the file first: GET $URL/<kind>, write its steps" >&2
    echo "  and fields into $BUNDLE/<kind>.toml until this reads equal." >&2
    fi
    if [ -n "$held_drift" ]; then
        [ -z "$open_drift" ] || echo "" >&2
        echo "  HELD — nothing here asks for these to be published. A held row goes" >&2
        echo "  live at a deliberate publish, a person's decision, and until then" >&2
        echo "  the tree is ahead of live by design ($HOLDS_REL/README.md):" >&2
        echo "" >&2
        # awk on the tab, not `read`: IFS whitespace would fold an empty
        # cell and show what lifts a hold as its why.
        printf '%s\n' "$holds_out" | awk -F'\t' -v want="$held_drift" '
            BEGIN { n = split(want, w, "\n"); for (i = 1; i <= n; i++) drifting[w[i]] = 1 }
            $1 == "held" && ($2 in drifting) {
                printf "    %s — %s\n      why: %s\n      what lifts it: %s\n", $2, $3, $4, $5
            }' >&2
    fi
fi

# A file that makes NO claim about a field is not drift — the row can
# hardly disagree with a sentence nobody wrote — but it is the quiet
# half of the same gap: nothing would compare that field ever again.
# Counted and named, because silence that nobody can see is how this
# class of defect gets in.
absent_lines=$(printf '%s\n' "$fields_out" | LC_ALL=C sed -n 's/^ABSENT\t//p')
if [ -n "$absent_lines" ]; then
    echo "  $(printf '%s\n' "$absent_lines" | wc -l | tr -d ' ') field(s) the file does not claim, and so cannot be compared:" >&2
    printf '%s\n' "$absent_lines" | while IFS=$'\t' read -r kind field; do
        echo "    $BUNDLE/$kind.toml has no \`$field\`" >&2
    done
fi

# Two derived lists the tail prints, computed here so the JSON report
# can carry them too. Neither is a failure (their comment is below,
# where they are printed).
pending=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$bundle_kinds") | LC_ALL=C sed '/^$/d')
tenant_lines=$(for f in "${tenant_files[@]}"; do
    all=$(tenant_kinds_of "$f" | LC_ALL=C sort -u | LC_ALL=C sed '/^$/d')
    [ -n "$all" ] || continue
    absent=$(LC_ALL=C comm -13 <(printf '%s\n' "$live_kinds") <(printf '%s\n' "$all") | LC_ALL=C sed '/^$/d' | wc -l | tr -d ' ')
    total=$(printf '%s\n' "$all" | wc -l | tr -d ' ')
    printf '%s\t%s\t%s\n' "$f" "$absent" "$total"
done)

# The verdict, decided once so the report and the exit cannot disagree.
verdict=0
if [ "$problems" -gt 0 ]; then verdict=1
elif [ "$drift_n" -gt 0 ] && [ "$REQUIRE_LIVE" -eq 1 ]; then verdict=2
fi
if [ -n "$REPORT_JSON" ]; then
    write_report "$REPORT_JSON" "$verdict" || {
        echo "$NAME: the JSON report could not be written to $REPORT_JSON — the caller asked for the record and did not get it" >&2
        exit 1
    }
fi

[ "$problems" -eq 0 ] || exit 1

# A drift is a verdict only for a caller that can act on it.
if [ "$drift_n" -gt 0 ] && [ "$REQUIRE_LIVE" -eq 1 ]; then
    echo "$NAME: $fields_compared kinds compared, $drift_n field(s) adrift — exiting 2 because --require-live was asked for a verdict" >&2
    exit 2
fi

live_n=$(printf '%s\n' "$live_kinds" | wc -l | tr -d ' ')
authored_n=$(printf '%s\n' "$authored" | wc -l | tr -d ' ')
if [ "$drift_n" -eq 0 ]; then
    msg="$NAME: OK — $live_n admitted kinds, $authored_n authored, $fields_compared live rows agree with their file"
else
    msg="$NAME: $live_n admitted kinds, $authored_n authored — $drift_n field(s) or step facet(s) adrift across $fields_compared compared (named above; REPORTED, not failed)"
fi
[ ${#EXEMPT[@]} -eq 0 ] || msg="$msg, ${#EXEMPT[@]} exempt (${EXEMPT[*]})"
lint_scanned "$NAME" "$authored_n" "authored kind(s) compared against $live_n admitted"
echo "$msg"

# Neither line below is a failure. A platform kind with no live row is
# the tree ahead of the deployment, in the window between a protocol
# car's merge and the converge + seed behind it. A tenant kind with no
# live row is a tenant this deployment does not run, which is the
# permanent and correct state for one of the two tenants this tree
# ships — counted rather than named, so a standing fact cannot train
# anyone to skim past the line above it.
[ -z "$pending" ] || printf '  authored in the bundle, not yet admitted (awaiting converge + seed): %s\n' "$(printf '%s\n' $pending | tr '\n' ' ')"
[ -z "$tenant_lines" ] || printf '%s\n' "$tenant_lines" | while IFS=$'\t' read -r f absent total; do
    [ "$absent" -eq 0 ] || printf '  %s: %s of %s kinds not admitted here (a tenant this deployment does not run)\n' "$f" "$absent" "$total"
done
exit 0
