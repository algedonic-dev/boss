#!/usr/bin/env bash
#
# prepared-deposit-keys.sh — the public half of every deposit key the
# credential broker has prepared for boss-gcp, and the whole line of the
# break-glass deposit key its own Job records (below), read off the
# system of record, one per line, for audit-deposit-keys.sh's
# --prepared-key.
#
#   prepared-deposit-keys.sh            the keys, one per line
#   prepared-deposit-keys.sh --kinds    the `<protocol kind> <subject>`
#                                       pairs the rules name, one per line
# BOSS_GCP_PREPARED_KEYS_CACHE names an optional private projection file;
# boss-gcp's converge supplies /var/cache/boss/prepared-deposit-keys.json.
#
# WHY IT EXISTS (backlog 77920d6a; review 728c6f76 of car 3cd92f7a). The
# deposit-key audit recognised a receiver line by the text of its forced
# command, and sshd hands that command to the account's shell, which
# reads `lib'exec'`, `bo\ss`, `$L` or `libexe?` as the root door while
# the text match reads none: such a line with no restrict read clean
# while sshd granted a pty and forwarding, and the deposit key placed
# bare, with no confined twin, read as one of the owner's own logins.
# Car 5cc2731a taught the audit to judge every line carrying a key it is
# told is a deposit key (`--prepared-key`), by fingerprint, whatever its
# command says. This is where boss-gcp's converge learns which keys
# those are: the ones the prepare packets recorded, never a list typed
# into the tree, because a key is minted at run time and only its
# packet holds its public half.
#
# WHICH PACKETS. Every rule under infra/dispatcher/rules whose handler is
# credential.prepare.ssh-deposit with receiver_host boss-gcp names one
# deposit key for this host: its args' protocol_kind is the packet kind,
# and its `when` (`subject_id = "<id>"`) the packet's subject. Read from
# the rules, never restated here, so a new deposit key for this host is
# judged on the first tick after its rule lands (CLAUDE.md 9a). A rule
# with that handler whose host, kind or subject this cannot read is
# UNVERIFIED: a deposit key it cannot place is one it cannot look for.
#
# WHICH KEYS. Every `public_key` on the issue or install step of every
# such packet, whatever the packet's or the step's status: the broker
# writes the pair's public half there (the enroll step's procedure sends
# the signer to the install step's to compare), and a key minted for a
# receiver is never anyone's login, so a revoked or superseded one stays
# prepared — a line still carrying it is judged by it until it is gone.
# A completed issue or install step that records no key is UNVERIFIED:
# that key exists and cannot be looked for.
#
# AND THE BREAK-GLASS DEPOSIT KEY, WHICH NO BROKER PREPARES (backlog
# 403d9d7e). The break-glass kubeconfig reaches boss-gcp over a key the
# boss-break-glass-deposit CronJob mints itself
# (infra/cluster/break-glass-deposit.sh), so it has no prepare packet,
# no rule above names it, and its line was judged by its command text
# alone — the gap 77920d6a closed for the others. Its record is the
# Job's own: every run that holds the key prints its whole
# authorized_keys line, `command="…ops-credential-recv kubeconfig",
# restrict <key> <comment>`, and the run that minted it printed the
# same line on its authorize RED; boss-chore.sh keeps both whole on the
# run step's `output` of a BREAK_GLASS_KIND packet. So every such line
# on every such packet is read, and handed WHOLE — options and key, the
# comment dropped — which binds the key to the kubeconfig receiver
# (audit-deposit-keys.sh, A PREPARED KEY IS BOUND TO ITS OWN RECEIVER):
# the same key forced to the machine-token door, which the text audit
# read clean, is a finding. A line naming the options with no key after
# them (the Job's own remedy text) names nothing.
#
# CORRECTING THE RECORD (backlog 203c2a72). Completed output stays
# immutable. deposit-key-records.py applies the authorised boss correct
# entries beside it, including withdrawals, before parsing keys. It
# names the packet, offending public value and correction door when a
# recorded key cannot be parsed. Added records still form a union.
# Each corrected identity must already occur in that step's original
# record: corrections may retract bad values or repair commentary, but
# a new key needs its own broker record. A generic job-update right
# cannot introduce another key through a correction (review 1249eac7).
#
# WARM READS. The optional cache preserves that historical union rather
# than forgetting a superseded key still on an authorised line. Its
# frontier comes from server step completions, inclusively with a one
# second overlap. Every tick also rereads open and corrected packets,
# so a late correction or withdrawal on an old run reaches the audit.
# A known cached identity is read on the same connection before reuse;
# empty answers alone prove neither health nor the right deployment.
# Redundant completed packets compact to key witnesses. A correction
# to a retained witness reconstructs history before dropping its value,
# because another old packet may still name the same key.
# Cold reconstruction still needs the complete history, and the work
# grows with distinct keys and corrected packets, not just daily runs.
# The server's completion-time cursor is the ordering contract; this
# is not a substitute for an audit-sequence cursor across clock resets.
# A corrupt or foreign cache refuses instead of silently trusting it.
#
# NONE IS UNVERIFIED. The deposit runs daily and every run that holds a
# key names it, so no such packet, or packets naming no key, is a dark
# or wrong read of a key boss-gcp may hold.
#
# NO EVIDENCE IS NOT A PASS. stdout is the keys, exit 0; or ONE line,
# `deposit-key-audit: UNVERIFIED — <why>`, exit 2, which the converge
# carries to its packet whole: no rules, no address, a read that is not
# whole (read_job_pages compares rows to `.total`), a key with a line
# break, a key sshd's parser cannot read, or no key at all. That last is
# a refusal, not an empty list:
# the record holds a prepared key for each deposit key boss-gcp has ever
# been given, and a read that finds none is a dark or wrong answer, the
# one shape a wrong target takes here (CLAUDE.md §Doors). The audit then
# reads the files without the keys, and its clean is not a clean.
#
# THE READ is signed as automation:boss-gcp-converge at the read role
# (sor_reader_header) and presents the machine token where
# machine_token_header allows. BOSS_GCP_PREPARED_KEYS_URL names another
# jobs list (a test's file:// fixture: curl drops a file URL's query,
# which is why the rows are selected by kind and subject here as well as
# by the query); BOSS_GCP_PREPARED_KEY_RULES another rules directory.
set +x
set -uo pipefail
export LC_ALL=C

ME=deposit-key-audit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
RULES="${BOSS_GCP_PREPARED_KEY_RULES:-$INFRA/dispatcher/rules}"
HOST=boss-gcp
# The break-glass deposit's record (above): the packet kind its chore
# files, the subject boss-maintenance-wrap.sh gives every maintenance
# packet, and the options its Job prints in front of the key —
# prepared_deposit_keys_sh.rs holds the kind equal to the manifest's
# chore and the options equal to the Job's FORCED.
BREAK_GLASS_KIND=maintenance-break-glass-deposit
BREAK_GLASS_SUBJECT="infra/$BREAK_GLASS_KIND"
BREAK_GLASS_FORCED='command="exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig",restrict'

unverified() {
    echo "$ME: UNVERIFIED — $1, so the deposit keys prepared for $HOST cannot be named and a line carrying one is judged by its text alone"
    exit 2
}
# Printable bytes, bounded: an error text may carry anything.
shown() { printf '%s' "$1" | tr -c '[:print:]' '?' | cut -c1-200; }

MODE=keys
case "${1:-}" in
    "") ;;
    --kinds) MODE=kinds ;;
    *) unverified "usage: $(basename "$0") [--kinds]" ;;
esac

work="$(mktemp -d)" || unverified "no scratch directory could be made"
# Set BEFORE the first header is made: secret-header.sh chains its own
# cleanup in front of this trap.
trap 'rm -rf -- "$work"' EXIT

command -v python3 >/dev/null 2>&1 || unverified "python3, which reads the dispatcher rules, is not on this host"
[ -d "$RULES" ] || unverified "$RULES, the dispatcher rules that name the deposit keys, is not a directory"
if ! python3 - "$RULES" "$HOST" >"$work/pairs" 2>"$work/pairs.err" <<'PY'; then
import pathlib, re, sys, tomllib

rules, host = pathlib.Path(sys.argv[1]), sys.argv[2]

def literal(value):
    # An args value is an expression; a string literal is `"<text>"`.
    m = re.fullmatch(r'"([^"\\]*)"', value) if isinstance(value, str) else None
    return m.group(1) if m else None

files = sorted(rules.glob("*.toml"))
if not files:
    sys.exit(f"no rule file under {rules}")
pairs = set()
for path in files:
    try:
        doc = tomllib.loads(path.read_text())
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
        sys.exit(f"{path.name} cannot be read as TOML ({e})")
    for rule in doc.get("rule", []):
        for do in rule.get("do", []):
            if do.get("handler") != "credential.prepare.ssh-deposit":
                continue
            name = f"rule {rule.get('name', '?')} ({path.name})"
            args = do.get("args", {})
            receiver = literal(args.get("receiver_host"))
            if receiver is None:
                sys.exit(f"{name} prepares a deposit key and names no receiver_host this reads")
            if receiver != host:
                continue
            kind = literal(args.get("protocol_kind"))
            when = re.fullmatch(r'subject_id = "([A-Za-z0-9._-]+)"', rule.get("when", ""))
            if not kind or not re.fullmatch(r"[a-z0-9-]+", kind) or not when:
                sys.exit(f"{name} prepares a {host} deposit key under a protocol kind or subject this cannot read")
            pairs.add(f"{kind} {when.group(1)}")
if not pairs:
    sys.exit(f"no rule under {rules} prepares a deposit key for {host}")
print("\n".join(sorted(pairs)))
PY
    unverified "the dispatcher rules could not be read: $(shown "$(tail -n 1 "$work/pairs.err")")"
fi
if [ "$MODE" = kinds ]; then
    cat "$work/pairs"
    exit 0
fi

# shellcheck source=infra/lib/sor.sh
. "$INFRA/lib/sor.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"
# shellcheck source=infra/lib/jobs-pages.sh
. "$INFRA/lib/jobs-pages.sh"
# shellcheck source=infra/lib/jq.sh
. "$INFRA/lib/jq.sh"

BASE="${BOSS_GCP_PREPARED_KEYS_URL:-}"
if [ -z "$BASE" ]; then
    [ -n "${BOSS_JOBS_URL:-}" ] || unverified "BOSS_JOBS_URL is unset (${BOSS_SOR_ENV:-/etc/boss/sor.env}), so the prepare packets cannot be read"
    BASE="$BOSS_JOBS_URL/api/jobs"
fi
MT=""
case "$BASE" in
    http://* | https://*) machine_token_header MT "$BASE" || MT="" ;;
esac
READER="$(sor_reader_header automation:boss-gcp-converge)"
job_pages_get() { # <query>
    curl -q -fsS --max-time 15 --max-redirs 0 ${MT:+-H "$MT"} -H "x-boss-user: $READER" "$BASE?$1"
}

# The completed record is immutable; only its correction list can grow.
# Keep the historical union locally, and refresh new completions, open
# packets and ALL corrected packets (including a late withdrawal on an
# old run). Cold reads still reconstruct it whole; a refused read never
# replaces the cache. A file:// snapshot has no cursor contract.
PROJECTOR="$HERE/deposit-key-records.py"
CACHE="${BOSS_GCP_PREPARED_KEYS_CACHE:-}"
printf '%s\n' "$BASE" "$BREAK_GLASS_FORCED" >"$work/context"
cat "$work/pairs" >>"$work/context"
printf '%s %s\n' "$BREAK_GLASS_KIND" "$BREAK_GLASS_SUBJECT" >>"$work/context"
if [ -n "$CACHE" ]; then
    mkdir -p -- "$(dirname "$CACHE")" || unverified "cache directory could not be made"
    exec 9>"$CACHE.lock" || unverified "cache lock could not be opened"
    flock -w 5 9 || unverified "another deposit-key read still holds the cache"
    # An old API that ignores either predicate must not advance a cursor.
    for control in 'step_completed_since=2999-01-01T00:00:00Z' 'metadata_has=deposit_key_control_never_written'; do
        read_job_pages "kind=$BREAK_GLASS_KIND&subject_id=$BREAK_GLASS_SUBJECT&$control" "$work/control" >"$work/control.json" \
            || unverified "the cache predicate control could not be read whole"
        [ "$(jq length "$work/control.json")" = 0 ] \
            || unverified "the API ignores $control; no cached audit is possible"
    done
fi
since="$(python3 "$PROJECTOR" load "$CACHE" "$work/context" "$work/cached.json" 2>"$work/project.err")" \
    || unverified "$(cat "$work/project.err")"
if [ -n "$since" ]; then
    # Empty deltas cannot distinguish quiet from the wrong deployment.
    # Re-read one identity the cache already holds on THIS connection.
    known="$(jq -r '.[0].id' "$work/cached.json")"
    encoded="$(jq -rn --arg id "$known" '$id|@uri')"
    curl -q -fsS --max-time 15 --max-redirs 0 ${MT:+-H "$MT"} -H "x-boss-user: $READER" "$BASE/$encoded" >"$work/witness.json" 2>"$work/witness.err" \
        || unverified "cached packet $known could not be re-read on this connection"
    jq_doc_file "$work/witness.json" \
        || unverified "this connection answered cached packet $known with no document"
    jq -e --arg id "$known" '.id == $id and (.steps|type)=="array"' "$work/witness.json" >/dev/null \
        || unverified "this connection did not answer with cached packet $known"
    jq '[.]' "$work/witness.json" >"$work/witness-rows.json"
fi
cat "$work/pairs" >"$work/queries"
printf '%s %s\n' "$BREAK_GLASS_KIND" "$BREAK_GLASS_SUBJECT" >>"$work/queries"
inputs=("$work/cached.json")
[ -z "$since" ] || inputs+=("$work/witness-rows.json")
n=0
while read -r kind subject; do
    n=$((n + 1))
    query="kind=$kind&subject_id=$subject"
    read_job_pages "$query${since:+&step_completed_since=$since}" "$work/page-$n" >"$work/rows-$n.json" \
        || unverified "the $kind packets for $subject could not be read whole from $(shown "${BASE%%\?*}") ($(shown "$(cat "$work/page-$n".page-*.stderr 2>/dev/null | tail -n 1)"))"
    inputs+=("$work/rows-$n.json")
    if [ -n "$since" ]; then
        for predicate in 'metadata_has=corrections' 'status=open'; do
            read_job_pages "$query&$predicate" "$work/page-$n-extra" >"$work/rows-$n-$predicate.json" \
                || unverified "the $kind $predicate packets could not be read whole"
            inputs+=("$work/rows-$n-$predicate.json")
        done
    fi
done <"$work/queries"
if [ -n "$since" ]; then
    rescan="$(python3 "$PROJECTOR" rescan "${inputs[@]}" 2>"$work/project.err")" \
        || unverified "$(cat "$work/project.err")"
    if [ "$rescan" = yes ]; then
        # A correction changed a retained witness. Reconstruct before
        # dropping its old value: another historical copy may name it.
        inputs=()
        n=0
        while read -r kind subject; do
            n=$((n + 1))
            read_job_pages "kind=$kind&subject_id=$subject" "$work/rescan-$n" >"$work/rescan-$n.json" \
                || unverified "the $kind history could not be reconstructed after a witness correction"
            inputs+=("$work/rescan-$n.json")
        done <"$work/queries"
    fi
fi
python3 "$PROJECTOR" merge "${inputs[@]}" >"$work/rows.json" 2>"$work/project.err" \
    || unverified "$(cat "$work/project.err")"
# Preserve the selected-parser refusal before any keys are printed.
SSH_KEYGEN="${BOSS_DEPOSIT_AUDIT_SSH_KEYGEN:-ssh-keygen}"
command -v "$SSH_KEYGEN" >/dev/null 2>&1 \
    || unverified "the key parser $SSH_KEYGEN is not on this host"
python3 "$PROJECTOR" project "$work/rows.json" "$work/pairs" "$BREAK_GLASS_KIND" "$BREAK_GLASS_SUBJECT" "$BREAK_GLASS_FORCED" "$CACHE" "$work/context" "$SSH_KEYGEN" >"$work/keys" 2>"$work/project.err" \
    || unverified "$(cat "$work/project.err")"
cat "$work/keys"
