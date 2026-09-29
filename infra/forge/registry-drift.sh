#!/usr/bin/env bash
# registry-drift.sh — the converge's last phase: compare the LIVE
# registries with the tree they were just converged from, and file each
# disagreement as ONE backlog-item that names which side is ahead and the
# command that resolves it (design d349e0ba car 2, backlog b79054b2).
#
# WHY HERE, AND NOT IN A GATE
# ---------------------------
# Two lints compare a tree with live state:
# `the-live-protocols-are-the-authored-protocols` (the workflow registry)
# and `the-live-rules-are-the-authored-rules` (the dispatcher's rules).
# Both used to run in every gate's pre-flight, and from 2026-09-21 to
# 2026-09-28 they failed 14 of 92 failed gates (15%) — none caused by the
# car under test. Gate 2f82e7fa went red because train #782 landed
# `break-glass-enrolment`, the converge seeded it live, and the car's
# older base had no file for it: the age of a base decided a verdict.
# Car 1 of the design took both out of the gate (`# consist: skip`). The
# comparison stays right; the MOMENT was wrong. After a converge — once
# the image is rolled and the seeds have run in its boot — is the one
# moment tree, image and live registry are supposed to agree, and this
# host holds the converged checkout. So the comparison runs here.
#
# WHAT IT FILES
# -------------
# The lints are the comparators; nothing is re-derived here (§9a). The
# protocols lint is run with `--require-live --report-json` (the mode
# infra/protocol-drift.sh already files from), the rules lint with
# `--findings`. Each disagreeing kind or rule becomes one item, and which
# side is ahead is READ from the tree and the registry, never guessed:
#
#   protocol, field drift   the file's last commit on this checkout
#                           against the live version's `created_at`:
#                           file newer AND live vN is the file's previous
#                           revision = tree ahead by an edit (diff first,
#                           then `boss workflow publish <kind> <file>`);
#                           file newer otherwise = both moved, side not
#                           measured; row newer = live ahead (write the
#                           row back in a car). By date and revision,
#                           because the publish records no lineage yet —
#                           9235802a makes it exact.
#   protocol, live, no file git says the file was DELETED = tree ahead by
#                           a retirement (e0254197 folds in here: the
#                           car lands first, and this files the retire);
#                           never had one = live ahead.
#   protocol, file, no row  tree ahead: the seed never inserted it.
#   rule the image does     live ahead: write its file in a car.
#     not author
#   the rules lint's system failures (wrong surface, withheld, zero rules,
#     an unreadable or empty authored registry) — one item per check.
#
# It never PUBLISHES. Protocols-as-data Q1, as David answered it, is
# insert-if-missing and nothing else (boss_platform_workflow_seed.rs);
# a machine publish would put an automation actor on the workflow
# registry's publish authority. Until Q1 says otherwise the machine asks
# for the publish BY NAME and a person runs it.
#
# ONE ITEM PER DISAGREEMENT, NOT PER CONVERGE. Every item carries
# `registry_drift` (`protocol:<kind>`, `rule:<name>`, `rules-check:
# <check>`) and an OPEN item with the same key is the item: a second
# converge finding the same disagreement files nothing. If the open list
# cannot be read whole, NOTHING is filed — a duplicate per converge (~28
# a day) is worse than one converge's silence, and the next converge
# re-derives the finding from state, so no only-copy is lost. The same
# holds for a read that answers but cannot be CONFIRMED: the open backlog
# is asked for its total first, and a total of zero is a dark read (a
# denied scope, a wrong or empty instance), never an empty queue.
#
# CLOSING AN ITEM DOES NOT SILENCE IT. The dedup reads OPEN items only, so
# closing one wontfix (declined, stale, duplicate) while the disagreement
# stands makes the NEXT converge file it again. That is deliberate — the
# registries still disagree, and this phase exists to say so — and the
# way to stop it is to resolve the disagreement (publish, retire, or
# write the row back), not to close the item.
#
# BOTH SIDES CAN MOVE. The date says which side changed LAST, not that
# the other did not: a car based before live vN's publish can land after
# it. So "tree ahead by an edit" also requires live vN to BE the file's
# previous revision (the lint's --compare, asked of that revision); when
# it is not, the item reads "both moved — side not measured". Every
# publish command names live vN's created_at (the registry records no
# publisher; authoring_job_id stands in) and says DIFF FIRST. On a SHALLOW
# checkout a retirement older than its depth is invisible, so a live kind
# with no file there is "side not measured", never "write it back".
#
# NEVER FAILS THE CONVERGE. Drift is work to queue, not a deploy failure,
# and a loop that deploys the fix must owe nothing to what it watches
# (CLAUDE.md §Diagnosis, "an arm that needs the patient is not an arm").
# An unreachable registry, a refused filing, a missing tool: each is said
# on the last line and the exit is 0. Only a usage error exits 2.
#
# USAGE
#   registry-drift.sh [--repo DIR]
#
# --repo is the converged checkout whose lints and bundle are read
# (default: the one this script is in). BOSS_JOBS_URL names the system
# of record (/etc/boss/sor.env through infra/lib/sor.sh); the rules are
# read at BOSS_DISPATCHER_URL, else that host on sor-ports.env's
# dispatcher port — the LAN machine door. BOSS_MACHINE_TOKEN, when set,
# rides in a header file (infra/lib/secret-header.sh). The last line is
#   registry-drift: <summary>
# which the converge records on its packet as `registry_drift`.

set -uo pipefail
export TZ=UTC

NAME="registry-drift"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SELF_DIR/../.."
PLINT="infra/lint/the-live-protocols-are-the-authored-protocols.sh"
RLINT="infra/lint/the-live-rules-are-the-authored-rules.sh"
ACTOR="automation:cluster-deploy-runner"
WRITER='{"id":"'"$ACTOR"'","role":"platform-admin","access_tier":"operator"}'

usage() { sed -n '/^# USAGE/,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2; }
while [ $# -gt 0 ]; do
    case "$1" in
        --repo) REPO="${2:?--repo needs a directory}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "$NAME: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
done

# The summary is the LAST line, and every way out goes through it.
finish() { echo "$NAME: $1"; exit 0; }

work=$(mktemp -d) || finish "nothing compared — no writable temp dir under ${TMPDIR:-/tmp}"
trap 'rm -rf "$work"' EXIT

for tool in curl python3 git; do
    command -v "$tool" >/dev/null 2>&1 || finish "nothing compared — no $tool on PATH"
done
[ -f "$REPO/$PLINT" ] && [ -f "$REPO/$RLINT" ] \
    || finish "nothing compared — $REPO does not carry $PLINT and $RLINT"

# The machine token rides to curl in a 0600 file, never in its argv
# (backlog 5f3ad356). Made here, in the script's own shell, after the
# trap above.
# shellcheck source=infra/lib/secret-header.sh
. "$SELF_DIR/../lib/secret-header.sh" || finish "nothing compared — infra/lib/secret-header.sh is missing"
MT_HDR=""
secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    || finish "nothing compared — BOSS_MACHINE_TOKEN is set and its header file could not be written"

# The system of record — no fallback address (infra/lib/sor.sh).
# shellcheck source=infra/lib/sor.sh
. "$SELF_DIR/../lib/sor.sh"
[ -n "${BOSS_JOBS_URL:-}" ] || finish "nothing compared — BOSS_JOBS_URL is unset and $SOR_ENV names none"
if [ -z "${BOSS_DISPATCHER_URL:-}" ]; then
    # shellcheck source=infra/forge/probe-bin/sor-routes.sh
    . "$SELF_DIR/probe-bin/sor-routes.sh"
    ports=$(grep -Ev '^[[:space:]]*(#|$)' "$SELF_DIR/sor-ports.env" 2>/dev/null | tr -s '[:space:]' ' ')
    if port=$(sor_port_for_service dispatcher "$ports"); then
        BOSS_DISPATCHER_URL=$(sor_base_on_port "$BOSS_JOBS_URL" "$port")
    fi
fi
echo "$NAME: protocols read at $BOSS_JOBS_URL, rules at ${BOSS_DISPATCHER_URL:-<no dispatcher port in sor-ports.env>}"

# api METHOD PATH OUT [BODY_FILE] — one call, the body to OUT, the HTTP
# code on stdout (000 when nothing answered).
api() {
    local method=$1 path=$2 out=$3 body=${4:-} code send=()
    [ -z "$body" ] || send=(-H 'content-type: application/json' --data-binary "@$body")
    code=$(curl -sS -m 20 -X "$method" -o "$out" -w '%{http_code}' \
        -H "x-boss-user: $WRITER" ${MT_HDR:+-H "$MT_HDR"} ${send[@]+"${send[@]}"} \
        "$BOSS_JOBS_URL$path" 2>>"$work/curl.err") || true
    printf '%s' "${code:-000}"
}

# ---------------------------------------------------------------------------
# The two comparisons, through the lints themselves.
# ---------------------------------------------------------------------------
prc=0
BOSS_JOBS_URL="$BOSS_JOBS_URL" bash "$REPO/$PLINT" --require-live --report-json "$work/protocols.json" \
    > "$work/protocols.log" 2>&1 || prc=$?
rrc=0
if [ -n "${BOSS_DISPATCHER_URL:-}" ]; then
    BOSS_DISPATCHER_URL="$BOSS_DISPATCHER_URL" bash "$REPO/$RLINT" --findings "$work/rules.tsv" \
        > "$work/rules.log" 2>&1 || rrc=$?
else
    rrc=3
    echo "no dispatcher port in $SELF_DIR/sor-ports.env, and BOSS_DISPATCHER_URL is unset" > "$work/rules.log"
fi
# A lint that did not simply agree speaks in the journal, whole: its text
# is the diagnosis, and it lives nowhere else (§Diagnosis, quiet is a loan).
[ "$prc" -eq 0 ] || sed "s/^/$NAME: [protocols exit $prc] /" "$work/protocols.log" >&2
[ "$rrc" -eq 0 ] || sed "s/^/$NAME: [rules exit $rrc] /" "$work/rules.log" >&2

# The live rows' dates, for which side of a field drift is ahead. Only
# read when there is a drift to place; a failed read leaves the side
# unmeasured on the item, never guessed.
if [ -s "$work/protocols.json" ] && grep -q '"field"' "$work/protocols.json"; then
    code=$(api GET /api/workflows "$work/workflows.json")
    [ "$code" = "200" ] || { echo "$NAME: GET /api/workflows answered HTTP $code — a field drift's side is not measured" >&2; rm -f "$work/workflows.json"; }
fi

# ---------------------------------------------------------------------------
# Classify: one candidate item per disagreeing kind, rule or check.
# ---------------------------------------------------------------------------
HEAD_SHA=$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)
python3 - "$REPO" "$work" "$prc" "$rrc" "$HEAD_SHA" "$ACTOR" <<'PY' > "$work/classify.out" 2>&1
import datetime, json, os, subprocess, sys

repo, work, prc, rrc, head, actor = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5], sys.argv[6]
BUNDLE = "infra/platform/workflows"
RULES = "infra/dispatcher/rules"

def git(*args):
    try:
        out = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, timeout=30)
    except Exception:
        return ""
    return out.stdout.strip() if out.returncode == 0 else ""

def epoch(iso):
    try:
        return int(datetime.datetime.strptime(iso[:19], "%Y-%m-%dT%H:%M:%S")
                   .replace(tzinfo=datetime.timezone.utc).timestamp())
    except Exception:
        return None

def load(name):
    p = os.path.join(work, name)
    try:
        with open(p) as f:
            return json.load(f)
    except Exception:
        return None

live_rows = {}
wf = load("workflows.json")
rows = wf.get("workflows") if isinstance(wf, dict) else wf
for r in rows or []:
    if isinstance(r, dict) and r.get("status", "active") == "active" and "kind" in r:
        live_rows[r["kind"]] = r

items = []
def item(key, title, side, command, detail):
    items.append({"key": key, "title": title, "side": side, "command": command, "detail": detail})

# A SHALLOW checkout cannot say what it does not hold: a deletion older
# than its depth is simply absent from `git log`, so "never authored"
# would read as live ahead and advise writing a retired kind back.
shallow = git("rev-parse", "--is-shallow-repository") != "false"
PLINT = os.path.join(repo, "infra/lint/the-live-protocols-are-the-authored-protocols.sh")

def live_matches_revision(kind, rev):
    """Does live `kind` agree with the file as it stood at `rev`? The
    lint's own comparator (--compare) over the converged bundle with that
    one file swapped for its `rev` copy — the SAME judgement that found the
    drift, asked of the older text (§9a). True / False / None (unknown)."""
    live = os.path.join(work, "workflows.json")
    if not os.path.exists(live):
        return None
    old = git("show", f"{rev}:{BUNDLE}/{kind}.toml")
    if not old:
        return None
    tmp = os.path.join(work, f"prev-{kind}")
    try:
        subprocess.run(["cp", "-R", os.path.join(repo, BUNDLE), tmp], check=True, timeout=60)
        with open(os.path.join(tmp, f"{kind}.toml"), "w") as f:
            f.write(old + "\n")
        out = subprocess.run(["bash", PLINT, "--compare", tmp, live],
                             capture_output=True, text=True, timeout=300)
    except Exception:
        return None
    if out.returncode != 0:
        return None
    return not any(l.split("\t")[:2] == ["DRIFT", kind] for l in out.stdout.splitlines())

report = load("protocols.json")
states = {}
if isinstance(report, dict):
    n0 = len(items)
    for kind in report.get("unauthored") or []:
        path = f"{BUNDLE}/{kind}.toml"
        deleted = git("log", "--diff-filter=D", "--format=%h", "-1", "--", path)
        if not deleted and shallow:
            item(f"protocol:{kind}", f"Registry drift: protocol {kind} — side not measured",
                 f"side not measured: the registry admits {kind}, no file authors it, and this checkout is SHALLOW, "
                 "so whether the tree retired it or never had it is not in the history it holds",
                 f"read git log --diff-filter=D -- {path} on a full clone: a deletion means boss-api POST "
                 f"/api/workflows/{kind}/retire; none means write the live row to {path} in a car "
                 f"(boss-api GET /api/workflows/{kind})",
                 f"The live workflow registry admits {kind} and the converged tree has no file for it. The "
                 "converge's checkout is shallow, so a retirement older than its depth is invisible here; "
                 "advising the row be written back could resurrect a retired kind.")
        elif deleted:
            item(f"protocol:{kind}", f"Registry drift: protocol {kind} — tree ahead by a retirement",
                 f"tree ahead by a retirement: {path} was deleted in {deleted} and the kind is still live",
                 f"boss-api POST /api/workflows/{kind}/retire",
                 f"The live workflow registry admits {kind}, and the converged tree deleted its file in {deleted}. "
                 "A retiring car lands first and the converge files the retirement (design d349e0ba, e0254197); "
                 "retiring the row is the step that finishes it.")
        else:
            item(f"protocol:{kind}", f"Registry drift: protocol {kind} — live ahead",
                 f"live ahead: the registry admits {kind} and no file in the tree has ever authored it",
                 f"write the live row to {path} in a car — read it with: boss-api GET /api/workflows/{kind}",
                 f"The live workflow registry admits {kind}, published live and never written back. Publishing a "
                 "version live stays legal; leaving the tree unable to describe it is the gap.")
    for kind in report.get("pending") or []:
        path = f"{BUNDLE}/{kind}.toml"
        item(f"protocol:{kind}", f"Registry drift: protocol {kind} — tree ahead, never admitted",
             f"tree ahead: {path} authors {kind} and the registry has no row for it after the seed",
             f"boss workflow publish {kind} {path}",
             f"The converged tree authors {kind} and the live registry admits no version of it, after a converge "
             "whose boot seed inserts every missing bundle kind. Read the boss pod's init log for the seed's answer.")
    by_kind = {}
    for d in (report.get("fields") or {}).get("drift") or []:
        by_kind.setdefault(d.get("kind"), []).append(d)
    for kind, drifts in sorted(by_kind.items()):
        path = f"{BUNDLE}/{kind}.toml"
        fields = ", ".join(sorted({d.get("field", "?") for d in drifts}))
        version = drifts[0].get("live_version")
        first = drifts[0]
        excerpt = (f"first difference in {first.get('field')}: tree «{first.get('tree_window', '')}» / "
                   f"live «{first.get('live_window', '')}»")
        row = live_rows.get(kind)
        # WHO published live vN is named in every publish command, and the
        # command says DIFF FIRST: a publish replaces the live row, so an
        # operator runs it knowing whose work it replaces. The registry
        # records no publisher, so the row's own authoring_job_id stands in.
        stamp = (f"live v{version}: created_at {row.get('created_at') or 'unknown'}, created_by not recorded "
                 f"by the registry (authoring_job_id {row.get('authoring_job_id') or 'none'})") if row \
            else f"live v{version}: its row could not be read"
        publish = (f"diff first — {stamp}; boss-api GET /api/workflows/{kind} against {path}, "
                   f"then: boss workflow publish {kind} {path}")
        export = f"write live v{version} back to {path} in a car — read it with: boss-api GET /api/workflows/{kind}"
        revs = git("log", "--format=%H", "--", path).split()
        file_ct = git("log", "-1", "--format=%ct", "--", path)
        live_ct = epoch(row.get("created_at", "")) if row else None
        if file_ct.isdigit() and live_ct is not None:
            file_ct = int(file_ct)
            when = (f"the file last changed at {datetime.datetime.fromtimestamp(file_ct, datetime.timezone.utc):%Y-%m-%dT%H:%MZ}, "
                    f"live v{version} was published at {row.get('created_at', '')[:16]}Z")
            if file_ct <= live_ct:
                title, side, cmd = "live ahead", f"live ahead: {when}", export
            else:
                # The date says the tree moved after live vN. That is
                # "tree ahead" only if live vN IS the file's previous
                # revision — else a car based before vN's publish landed
                # after it, and publishing the file would overwrite newer
                # live work: BOTH moved.
                prev = revs[1] if len(revs) > 1 else None
                same = live_matches_revision(kind, prev) if prev else None
                if same:
                    title, side, cmd = ("tree ahead by an edit",
                                        f"tree ahead by an edit: {when}, and live v{version} is the file's previous "
                                        f"revision ({prev[:8]})", publish)
                else:
                    why = (f"live v{version} is not the file's previous revision ({prev[:8]})" if same is False
                           else "whether live vN is the file's previous revision could not be read"
                           + (" (the checkout is shallow)" if shallow else ""))
                    title, side, cmd = ("both moved — side not measured",
                                        f"both moved — side not measured: {when}, but {why}; publishing the file "
                                        "could overwrite newer live work",
                                        f"{publish} — only if the diff shows nothing live to keep; else {export}")
        else:
            why = "the file has no commit on this checkout" if not file_ct.isdigit() else "the live row's date could not be read"
            title, side, cmd = "side not measured", f"side not measured: {why}", f"{publish} (tree ahead), or {export} (live ahead)"
        item(f"protocol:{kind}", f"Registry drift: protocol {kind} — {title}", side, cmd,
             f"{kind}: {len(drifts)} field(s) disagree between {path} and live v{version}: {fields}. {excerpt}. "
             "Which side is ahead is read by date and by the file's previous revision until the publish "
             "records its lineage (9235802a).")
    states["protocols"] = f"{len(items) - n0} disagreement(s)" if len(items) > n0 else "agree"
elif prc != 0:
    states["protocols"] = f"not compared (exit {prc})"
else:
    states["protocols"] = "not compared (the lint wrote no report)"

RULE_CHECKS = {
    "wrong-surface": ("the read surface answered 200 with no rules array",
                      "read what answered: boss-api GET /api/dispatcher/rules — a 200 from the wrong surface or an error body"),
    "withheld": ("the dispatcher withheld its registry from the reader",
                 "check the reader's x-boss-user (infra/lint/lib/sor-read.sh signs as audit-readonly; backlog e76582c1)"),
    "zero-rules": ("the dispatcher enforces ZERO rules",
                   "read the dispatcher's boot log: its seed publishes infra/dispatcher/rules at boot, and zero rules runs zero side effects"),
    "authored-registry-unreadable": ("the deployment cannot read its own authored rule registry",
                                     "check the image carries /opt/boss/infra/dispatcher/rules and BOSS_DISPATCHER_RULES names it"),
    "authored-registry-empty": ("the deployment reports an EMPTY authored rule registry",
                                "check the image carries /opt/boss/infra/dispatcher/rules — a packaging fault"),
}
n0 = len(items)
lines = []
try:
    with open(os.path.join(work, "rules.tsv")) as f:
        lines = [l.rstrip("\n").split("\t", 1) for l in f if l.strip()]
except Exception:
    pass
for cells in lines:
    check, subject = cells[0], (cells[1] if len(cells) > 1 else "")
    if check == "unauthored-rule":
        path = f"{RULES}/{subject}.toml"
        item(f"rule:{subject}", f"Registry drift: dispatcher rule {subject} — live ahead",
             f"live ahead: the dispatcher enforces {subject} and its own image authors no file for it",
             f"write {path} in a car, carrying its why — read the live body with: boss-api GET /api/dispatcher/rules",
             f"The running dispatcher enforces {subject}, and the directory its image carries has no file for it, "
             "so a fresh database would not have the rule. Usually published live through POST /api/dispatcher/rules "
             "and never written back (backlog 8d471ec5); deleting a file is how a rule is retired.")
    else:
        what, cmd = RULE_CHECKS.get(check, (f"the rules check {check} failed", "read the lint's text in this converge's journal"))
        item(f"rules-check:{check}", f"Registry drift: dispatcher rules — {what}",
             f"the deployment: {what}", cmd, f"{what}: {subject}")
if lines:
    states["rules"] = f"{len(items) - n0} disagreement(s)"
elif rrc == 0:
    states["rules"] = "agree"
elif rrc == 1:
    states["rules"] = "exit 1 with no live finding (a tree failure; its text is in the journal)"
else:
    states["rules"] = f"not compared (exit {rrc})"

with open(os.path.join(work, "candidates.json"), "w") as f:
    json.dump(items, f)
print(f"protocols: {states['protocols']}; rules: {states['rules']}")
PY
[ $? -eq 0 ] || { sed "s/^/$NAME: [classify] /" "$work/classify.out" >&2; finish "nothing filed — the classification failed (above)"; }
STATES=$(tail -n 1 "$work/classify.out")
N=$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "$work/candidates.json") \
    || finish "$STATES; nothing filed — the candidates could not be read back"
[ "$N" -gt 0 ] || finish "$STATES; nothing to file"

# ---------------------------------------------------------------------------
# Dedup against what is OPEN — every page, or nothing is filed.
# ---------------------------------------------------------------------------
# A DARK READ IS NOT AN EMPTY QUEUE. A denied scope, or a wrong or empty
# instance, answers 200 `total: 0` — and read as "nothing open" that would
# refile every disagreement on every converge (~28 a day) while POST
# still works. So a KNOWN ANSWER is asked first, on the same connection
# with the same identity: the open backlog is never empty on an instance
# this runs against (it holds hundreds; measured 402 on 2026-09-28), so a
# total that is not a positive number means the read is not seeing the
# queue, and nothing is filed. The dedup read itself is narrowed to the
# items that carry the key (`metadata_has`), and every page is read.
: > "$work/open.tsv"
code=$(api GET "/api/jobs?kind=backlog-item&status=open&limit=1" "$work/control.json")
control=$(python3 -c 'import json,sys; print(int(json.load(open(sys.argv[1]))["total"]))' "$work/control.json" 2>/dev/null) || control=unreadable
[ "$code" = "200" ] || finish "$STATES; nothing filed — the known-answer read of the open backlog answered HTTP $code, so the dedup cannot be trusted"
case "${control:-empty}" in
    empty|*[!0-9]*|0) finish "$STATES; nothing filed — the open backlog read back ${control:-no total} item(s), and it is never empty: a dark or narrowed read, so the dedup cannot be trusted" ;;
esac
offset=0
open_read=""
while :; do
    code=$(api GET "/api/jobs?kind=backlog-item&status=open&metadata_has=registry_drift&limit=500&offset=$offset" "$work/page.json")
    if [ "$code" != "200" ]; then open_read="the open list answered HTTP $code at offset $offset"; break; fi
    read -r rows total < <(python3 - "$work/page.json" "$work/open.tsv" <<'PY'
import json, sys
try:
    doc = json.load(open(sys.argv[1]))
    data, total = doc["data"], int(doc["total"])
except Exception:
    print("x x"); sys.exit(0)
with open(sys.argv[2], "a") as out:
    for r in data:
        key = (r.get("metadata") or {}).get("registry_drift")
        if isinstance(key, str) and key:
            out.write(f"{key}\t{r.get('id', '')}\n")
print(len(data), total)
PY
)
    case "${rows:-x}:${total:-x}" in
        *[!0-9:]*|:*|*:) open_read="the open list answered a page with no data array or no total"; break ;;
    esac
    offset=$((offset + rows))
    [ "$offset" -lt "$total" ] || break
    [ "$rows" -gt 0 ] || { open_read="the open list stopped at $offset of $total"; break; }
done
[ -z "$open_read" ] || finish "$STATES; nothing filed — $open_read, so a filing could duplicate an open item"

# ---------------------------------------------------------------------------
# File what is not already open.
# ---------------------------------------------------------------------------
filed=0; already=0; refused=0
for i in $(seq 0 $((N - 1))); do
    key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[int(sys.argv[2])]["key"])' "$work/candidates.json" "$i")
    open_id=$(awk -F'\t' -v k="$key" '$1 == k { print $2; exit }' "$work/open.tsv")
    if [ -n "$open_id" ]; then
        echo "$NAME: $key — already open as ${open_id:0:8}, not filed again"
        already=$((already + 1)); continue
    fi
    python3 - "$work/candidates.json" "$i" "$ACTOR" "$HEAD_SHA" > "$work/item.json" <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))[int(sys.argv[2])]
actor, head = sys.argv[3], sys.argv[4]
description = (f"{c['detail']}\n\nWhich side is ahead: {c['side']}.\n\nResolve with: {c['command']}\n\n"
               f"Found by the cluster converge's last phase at {head[:12]} (infra/forge/registry-drift.sh, "
               "design d349e0ba). The converge files and does not publish: protocols-as-data Q1 is "
               "insert-if-missing. One item per disagreement: while this is open, later converges that find "
               "the same one file nothing.")
print(json.dumps({
    "kind": "backlog-item", "status": "open", "owner_id": actor, "priority": "standard", "tags": [],
    "subject": {"subject_kind": "custom", "id": "bosspipeline"},
    "title": c["title"],
    "metadata": {
        "registry_drift": c["key"], "side": c["side"], "resolve": c["command"],
        "description": description, "area": "delivery", "input_channel": "telemetry/monitoring",
        "filed_by": actor, "converged_head": head, "design": "d349e0ba",
    },
}))
PY
    code=$(api POST /api/jobs "$work/created.json" "$work/item.json")
    if [ "$code" = "201" ]; then
        id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id", "?"))' "$work/created.json" 2>/dev/null || echo "?")
        echo "$NAME: $key — filed ${id:0:8}"
        filed=$((filed + 1))
        # The next candidate with the same key (none today) sees it open.
        printf '%s\t%s\n' "$key" "$id" >> "$work/open.tsv"
    else
        echo "$NAME: $key — NOT filed, POST /api/jobs answered HTTP $code: $(head -c 300 "$work/created.json" 2>/dev/null)" >&2
        refused=$((refused + 1))
    fi
done
finish "$STATES; filed $filed, already open $already, not filed $refused"
