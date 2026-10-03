#!/usr/bin/env bash
# node-converge.sh — apply ONE node's declared Talos patch, through a
# rendered dry-run a passkey signs. The ops verbs `plan-a-node-converge`
# (--plan) and `node-converge` (the write).
#
#   node-converge.sh --plan <node> <tree-sha>
#   node-converge.sh <node> <tree-sha> <plan-sha256>
#
# WHY IT EXISTS (backlog 9d56c616; design 1bc4b4ed, David 2026-09-12).
# The design decided that cluster management runs on an internal host and
# that the ONE bounded Talos act is `node-converge <node>`: a dry run of
# the per-node file the repo declares (infra/cluster/talos/patches/, the
# non-secret entries; PKI stays in /etc/boss-ops), the diff recorded on
# the packet, then the apply. Until this verb that was `talosctl patch
# machineconfig` typed by hand with the admin talosconfig — on 2026-09-30
# for w-1's second NVMe (disk car 52ea56ac), David asked why the agent
# could not run it and chose "do both": he typed it once, and this is the
# door for every node change after it. Reboot, upgrade, reset and etcd
# membership NEVER become verbs, and this one cannot do any of them.
#
# THE SHAPE is design 17835005's, as shutdown-node's and
# set-longhorn-drain-policy's: the verb file declares requires_approval
# and names plan-a-node-converge as its plan_verb, so the runner renders
# this script's --plan onto the request's approve step and hands the
# write sha256 of the SIGNED plan. The write re-renders — a fresh dry run
# — and applies nothing unless today's plan hashes to the approved one.
#
# THE BOUND is this script's, never a parameter:
#   * THE NODE is one the estate registry declares (not retired, one IPv4
#     literal) with a Talos role, and the cluster agrees — the registry's
#     address as the Node's InternalIP, and a control-plane label exactly
#     when the registry says talos-control-plane (cluster-node-lib.sh's
#     node_require_talos and node_agree_cluster). A control plane is
#     admitted, and only this verb admits one: design 1bc4b4ed converges
#     every node that has a declaration, and bounds the ACT instead.
#     The live config must also name the node as its hostname
#     (check-declared.sh refuses any other).
#   * THE PATCH is infra/cluster/talos/patches/<node>.yaml read out of git
#     at the named commit — never a free path, never patch text from the
#     packet. The commit is a full 40-hex sha on the history this checkout
#     converged to (an ancestor of HEAD), and the file it holds must be
#     the very blob HEAD holds: applying an older declaration would
#     regress the node, so a declaration that changed since the named
#     commit is refused, naming the blob HEAD carries.
#   * THE DECLARATION IS IN CANONICAL FORM (check-declared.sh, review
#     a7fa61ec): byte-identical, less comments and blank lines, to the
#     one block form the check re-emits from what it parsed. The check
#     and Talos's yaml.v3 read different structure from one text three
#     times (a selector's text, a comment ended by LS, a `--- {…}` line
#     that was a whole document to Talos); canonical form closes that
#     class, so every bound below judges the structure Talos will apply.
#   * EVERY ENTRY IT DECLARES IS ONE THE READ-BACK CAN READ
#     (check-declared.sh --only-read-classes): an effect nobody could read
#     is an effect nobody could prove, so it is refused before any door
#     opens. A UserVolumeConfig document is read back by name (disk car
#     52ea56ac taught the check) and so may be applied — but only when
#     its diskSelector is in check-declared.sh's ALLOWLIST grammar:
#     `term (&& term)*`, each term exactly `!system_disk` or
#     `disk.<field> <op> <number or plain string>`, exactly one
#     `!system_disk`, and no parentheses, brackets, comments or newlines
#     (review be5ba8f7 finding 1: a text search for the term was bypassed
#     by a negated group, a compared group, a CEL comment, a string
#     literal and a list). Any other document kind is refused.
#   * NO REBOOT. Both the dry run and the apply run `--mode=no-reboot`,
#     so Talos itself refuses a change it cannot apply live; and the dry
#     run's own summary must read exactly "Applied configuration without
#     a reboot" or the plan is refused.
#   * NO SECOND COPY. Talos's strategic merge APPENDS a list entry it
#     cannot key (README.md beside the patches: that is how w-1's seed
#     file entry came to be doubled). So the dry run runs twice — once
#     with the patch, once with it applied twice — and when the two differ
#     the patch appends: it is then refused if ANY declared entry, in any
#     class, is already live (MATCH, DRIFT or DOUBLED), since which part
#     appends is not visible and applying it again could add a second
#     copy. The live entries are working config and the refusal says to
#     keep them: the remedy is a declaration narrowed to entries ABSENT
#     live, or a partial patch (a follow-up), never deleting a live entry
#     (review 9cb9af67, findings 1 and 2).
#   * ONLY THE KUBELET KEYS DECLARED TODAY under extraConfig (the image-GC
#     pair, check-declared.sh's EXTRA_CONFIG_KEYS): extraConfig passes any
#     KubeletConfiguration key to the kubelet, anonymous auth and
#     authorization mode among them (finding 10).
#   * SOMETHING TO DO. A dry run that changes nothing is refused — so an
#     applied plan re-renders to a refusal: at most once.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the node,
# its address, estate role and Node uid, the file, commit and blob, the
# one command, the declaration's own text, the node read against the
# declaration now (check-declared.sh), Talos's dry-run summary, whether
# the patch is idempotent, and Talos's config diff.
#
# NO CREDENTIAL IS EVER PRINTED. A machine config carries the cluster's
# CA keys and join tokens, and Talos's diff is a unified diff of the
# WHOLE config with three lines of context — context that can land on a
# token. So the diff is rendered with every context line reduced to its
# key (`token: …`), and a changed line whose key names a credential
# (token, key, crt, ca, secret, password, …), or whose value carries a
# PEM header or a long mixed-case base64 run, refuses the plan naming the
# line number and the key, never the value. talosctl's words on a failure
# are printed with any such run withheld. The live config itself is only
# ever piped into check-declared.sh, and its findings reach the plan and
# the failure line as a verdict and a key each — never the live values it
# prints beside them (review 9cb9af67, finding 4).
#
# THE WRITE re-renders, refuses (exit 78, naming both hashes, nothing
# done) unless today's plan hashes to the approved one, prints the plan
# (the capture before the act), and runs `talosctl -n <address> patch
# machineconfig --mode=no-reboot --patch=<the declaration>` through the
# ops_talosctl door (the talosconfig David placed in /etc/boss-ops).
# `patch machineconfig` is not conditional: Talos computes the delta
# against the config it holds at apply time, seconds after the re-render
# matched. With one writer (this verb, under a passkey) that window is
# accepted; a second writer in it would be applied over (review 9cb9af67,
# note 8).
#
# THE EFFECT IS READ BACK, and it is the NODE, not only its config: every
# BOSS_NODE_CONVERGE_POLL_S (default 5) up to BOSS_NODE_CONVERGE_WAIT_S
# (default 120, inside the verb's timeout), the live machine config
# through check-declared.sh against the declared blob, and then the Node:
# Ready=True, with its kubelet lease (kube-node-lease/<node>) renewed at
# least BOSS_NODE_CONVERGE_SETTLE_S (default 20) after the apply returned
# — a kubelet running after the restart the change causes, not a stale
# condition. The verdict is the read taken at the END of the bound, so a
# node back early is read again (a runtime can report NotReady a status
# update after the kubelet's first fresh lease; review be5ba8f7, finding
# 5): every successful run takes the whole bound. Only then is the line
# the verb file declares as `effect` printed; otherwise exit 1 saying the
# node is NOT proven, with what each read said (review 9cb9af67, finding
# 3). The check's exit 0 or 1 counts only beside its own summary line —
# a check that died printing nothing is not "no findings" (review
# be5ba8f7, finding 2).
#
# Exit 78 is a refusal (the request was wrong or no longer true); exit 1
# a step that failed, including a read that could not look.
set -uo pipefail

ME=node-converge
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/cluster-node-lib.sh
. "$HERE/cluster-node-lib.sh"

PLAN=0
if [ "${1-}" = "--plan" ]; then
    PLAN=1
    shift
    [ $# -eq 2 ] || node_refuse "usage: node-converge.sh --plan <node> <tree-sha> — the plan takes the node and the commit whose declaration it applies, and nothing else"
else
    [ $# -eq 3 ] || node_refuse "usage: node-converge.sh <node> <tree-sha> <plan-sha256> — this applies only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-node-converge verb)"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || node_refuse "the plan hash must be 64 hex characters, got '$3'"
    APPROVED="$3"
fi
NODE="$1"
SHA="$2"
node_check_id "$NODE"
[[ "$SHA" =~ ^[0-9a-f]{40}$ ]] \
    || node_refuse "the tree sha must be one full 40-hex commit (never an abbreviation, a branch or HEAD — a name that moves is not what a passkey can sign), got '$SHA'"
command -v jq >/dev/null 2>&1 || node_fail "jq is not on PATH — the registry cannot be read, and no evidence is not a node"
command -v git >/dev/null 2>&1 || node_fail "git is not on PATH — the declaration is read out of git at the named commit, and there is no other source"

WAIT_S="${BOSS_NODE_CONVERGE_WAIT_S:-120}"
POLL_S="${BOSS_NODE_CONVERGE_POLL_S:-5}"
# The kubelet lease must be renewed this long after the apply returned:
# twice its ~10 s renew interval, so a renewal by the kubelet Talos is
# about to restart cannot pass for one by the kubelet that came back.
SETTLE_S="${BOSS_NODE_CONVERGE_SETTLE_S:-20}"
case "${WAIT_S:-empty}${POLL_S:-empty}${SETTLE_S:-empty}" in
    *[!0-9]*) node_fail "BOSS_NODE_CONVERGE_WAIT_S, _POLL_S and _SETTLE_S must be whole seconds, got '$WAIT_S', '$POLL_S' and '$SETTLE_S'" ;;
esac

WORK="$(mktemp -d)" || node_fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/decl"

# The checkout the runner itself runs from (ops-runner.sh's OPS_REPO_ROOT
# is the same directory); BOSS_NODE_CONVERGE_TREE is the tests' seam.
TREE="${BOSS_NODE_CONVERGE_TREE:-$(cd "$HERE/../.." && pwd)}"
REL="infra/cluster/talos/patches/$NODE.yaml"
CHECK="$HERE/../cluster/talos/check-declared.sh"
[ -r "$CHECK" ] || node_fail "the read-back $CHECK is not in this checkout — an effect nobody can read is not applied"

# mask — talosctl's and the check's words, with anything shaped like a
# credential withheld: a long run of token characters, and the live line
# a parse error quotes (check-declared's "got '…'").
mask() {
    sed -E 's#[A-Za-z0-9+/=_-]{40,}#<withheld: a token-shaped run>#g; s#(, got ).*#\1<withheld: a line of the live config>#'
}

# --- the registry first: a node with no Talos role is refused before any
# --- door opens --------------------------------------------------------------
node_read_registry "$NODE"
node_require_talos "$NODE"

# --- the declaration, out of git at the named commit -------------------------
# The runner executes verbs AS ROOT and the checkout is its owner's; since
# git 2.35.2 a repository owned by somebody else is "dubious ownership"
# and every command refuses (delete-orphan-object.sh measured it,
# ops-request c9877f75). So every git read drops to the owner, read off
# the directory — never safe.directory, which would make a root write
# into a user's clone reachable by the next edit.
[ -d "$TREE" ] || node_fail "the checkout $TREE does not exist, so there is no declaration to read"
OWNER_UID="$(stat -c %u "$TREE" 2>/dev/null)"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ "$(id -u)" != "$OWNER_UID" ]; then
    { [ -n "$OWNER" ] && [ "$OWNER" != UNKNOWN ] && id -u "$OWNER" >/dev/null 2>&1; } \
        || node_fail "cannot resolve the owner of $TREE (stat says '${OWNER:-}', uid ${OWNER_UID:-?}) — there is no account to read its git as"
    OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
fi
g() {
    if [ "$(id -u)" = "$OWNER_UID" ]; then
        git -C "$TREE" "$@"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" git -C "$TREE" "$@"
    fi
}
g cat-file -e "$SHA^{commit}" 2> "$WORK/git.err" \
    || node_refuse "commit $SHA is not in the checkout $TREE — name a commit on the main this host converged to"
# An annotated tag's object id peels to its commit and would pass the
# line above, then print as "commit <tag id>" in the signed plan (review
# 9cb9af67, note 9): the name signed must BE the commit.
[ "$(g rev-parse --verify --quiet "$SHA^{commit}" 2>/dev/null)" = "$SHA" ] \
    || node_refuse "$SHA is not itself a commit (an annotated tag's id peels to one) — name the commit"
g merge-base --is-ancestor "$SHA" HEAD 2> "$WORK/git.err"
case $? in
    0) ;;
    1) node_refuse "commit $SHA is not on the history this checkout converged to (HEAD $(g rev-parse --short HEAD 2>/dev/null)) — a declaration applies only once it has landed and this host has it" ;;
    *) node_fail "git could not say whether $SHA is on the converged history: $(tr '\n' ' ' < "$WORK/git.err")" ;;
esac
BLOB="$(g rev-parse --verify --quiet "$SHA:$REL" 2>/dev/null)" \
    || node_refuse "commit $SHA declares nothing for $NODE: there is no $REL in its tree"
HEAD_BLOB="$(g rev-parse --verify --quiet "HEAD:$REL" 2>/dev/null)" \
    || node_refuse "this checkout's HEAD declares nothing for $NODE (no $REL), so the declaration at $SHA is not the current one"
[ "$BLOB" = "$HEAD_BLOB" ] \
    || node_refuse "the declaration at $SHA ($REL, blob $BLOB) is not the one HEAD $(g rev-parse HEAD 2>/dev/null) carries (blob $HEAD_BLOB) — it changed since, and applying the older one would regress $NODE. Name the commit that carries the current declaration"
[ "$(g cat-file -t "$BLOB" 2>/dev/null)" = blob ] || node_refuse "$REL at $SHA is not a file"
g cat-file blob "$BLOB" > "$WORK/decl/$NODE.yaml" 2> "$WORK/git.err" \
    || node_fail "could not read $REL at $SHA: $(tr '\n' ' ' < "$WORK/git.err")"
[ -s "$WORK/decl/$NODE.yaml" ] || node_refuse "$REL at $SHA is empty — there is nothing to converge"
# talosctl reads a --patch value that begins with @ as a PATH inside its
# container, which is exactly the free path this verb never takes.
[ "$(head -c 1 "$WORK/decl/$NODE.yaml")" != "@" ] || node_refuse "$REL at $SHA begins with @, which talosctl would read as a file path, not a patch"
PATCH="$(cat "$WORK/decl/$NODE.yaml")"

# check_declared <args…> — check-declared.sh against the declared blob,
# run FROM the scratch directory with a relative path: the check prints
# the declaration's path, and a mktemp name in the signed bytes would
# make every render of one true state a different plan.
check_declared() { (cd "$WORK" && BOSS_TALOS_PATCHES=decl bash "$CHECK" "$@"); }

# Every declared entry must be one the read-back can read.
check_declared --only-read-classes "$NODE" > "$WORK/classes.out" 2> "$WORK/classes.err"
case $? in
    0) ;;
    3 | 2) node_refuse "$REL at $SHA declares what check-declared.sh cannot read back, so its effect could not be proven and nothing is applied: $(cat "$WORK/classes.out" "$WORK/classes.err" | tr '\n' ' ')" ;;
    *) node_fail "check-declared.sh could not judge the declaration: $(cat "$WORK/classes.out" "$WORK/classes.err" | tr '\n' ' ')" ;;
esac

# --- the doors, and the cluster's agreement ----------------------------------
node_door
T="$(ops_talosctl)"
node_agree_cluster "$NODE"

# read_back <tag> — the live machine config, piped straight into the
# check against the declared blob and never written down: sets GET_RC and
# CHECK_RC, the findings in $WORK/<tag>.out.
read_back() {
    # shellcheck disable=SC2086 # T is one line the door prints to be word-split
    $T -n "$NODE_ADDRESS" get machineconfig -o yaml < /dev/null 2> "$WORK/$1.get" \
        | check_declared "$NODE" > "$WORK/$1.out" 2> "$WORK/$1.err"
    local st=("${PIPESTATUS[@]}")
    GET_RC="${st[0]}"
    CHECK_RC="${st[1]}"
}

# dry <n> — a dry run with the patch applied <n> times, in no-reboot mode;
# talosctl's words in $WORK/dry<n>.all (stdout, then stderr, where
# talosctl prints the mode details and the diff), its status in DRY_RC.
dry() {
    local args=() i
    for ((i = 0; i < $1; i++)); do args+=("--patch=$PATCH"); done
    # shellcheck disable=SC2086
    $T -n "$NODE_ADDRESS" patch machineconfig --mode=no-reboot --dry-run "${args[@]}" \
        < /dev/null > "$WORK/dry$1.out" 2> "$WORK/dry$1.err"
    DRY_RC=$?
    cat "$WORK/dry$1.out" "$WORK/dry$1.err" > "$WORK/dry$1.all"
}

# no_reboot_refusal <file> — true only when talosctl's words are Talos's
# own refusal of a change it cannot apply live: an InvalidArgument or
# FailedPrecondition status saying "without (a) reboot". Anything else
# that happens to mention a reboot — a timeout while a node reboots, a
# proxy's "rebooting" — is not that refusal, and says nothing about what
# was applied (review 9cb9af67, finding 7).
no_reboot_refusal() {
    grep -Eq 'code = (InvalidArgument|FailedPrecondition).*without (a )?reboot' "$1"
}

# judge_dry <n> — refuse a dry run that needs a reboot or says anything
# but "without a reboot", fail one that did not answer; then split it into
# $WORK/dry<n>.summary and $WORK/dry<n>.diff.
judge_dry() {
    if [ "$DRY_RC" -ne 0 ]; then
        if no_reboot_refusal "$WORK/dry$1.all"; then
            node_refuse "the declaration would need a REBOOT of $NODE: Talos, asked in --mode=no-reboot, refused it ($(mask < "$WORK/dry$1.all" | tr '\n' ' ')). A reboot is never this verb — nothing was applied"
        fi
        node_fail "talosctl -n $NODE_ADDRESS patch machineconfig --dry-run did not answer ($(mask < "$WORK/dry$1.all" | tr '\n' ' ')) — a dry run applies nothing"
    fi
    # talosctl may open its response with the node it answered for (its
    # PrintApplyResults does when the response carries a hostname): `<the
    # registry's address>: ` or `<node>: `, on the summary's first line.
    # Only those two prefixes are read; any other stays unparsed and fails
    # closed (review 9cb9af67, finding 6).
    awk -v sumf="$WORK/dry$1.summary" -v addr="$NODE_ADDRESS" -v name="$NODE" '
        s == 0 {
            v = $0
            if (index(v, addr ": ") == 1) v = substr(v, length(addr) + 3)
            else if (index(v, name ": ") == 1) v = substr(v, length(name) + 3)
            if (v ~ /^[[:space:]]*Dry run summary:[[:space:]]*$/) s = 1
            next
        }
        s == 1 && NF { v = $0; sub(/^[[:space:]]+/, "", v); sub(/[[:space:]]+$/, "", v); print v > sumf; s = 2; next }
        s == 2 && /^[[:space:]]*Config diff:[[:space:]]*$/ { s = 3; next }
        s == 3 { print }
        END { if (s != 3) exit 5 }' "$WORK/dry$1.all" > "$WORK/dry$1.diff"
    [ $? -eq 0 ] || node_fail "Talos's dry run did not read as a summary and a config diff, so whether $NODE would reboot is unknown — no evidence is not a pass, and nothing was applied (it printed $(wc -l < "$WORK/dry$1.all" | tr -d ' ') line(s), not rendered)"
    local summary
    summary="$(cat "$WORK/dry$1.summary")"
    case "$summary" in
        "Applied configuration without a reboot (skipped in dry-run)." | "Applied configuration without a reboot (skipped in dry-run)") ;;
        *) node_refuse "Talos's dry run for $NODE says '$summary', not 'Applied configuration without a reboot' — only a change Talos applies live is this verb. Nothing was applied" ;;
    esac
}

# render_diff — Talos's raw diff ($WORK/dry1.diff) as the plan shows it:
# headers and changed lines as they are, every context line reduced to its
# key. Refuses a changed line shaped like a credential, naming the line
# and the key. Sets PLUS and MINUS.
render_diff() {
    awk -v cntf="$WORK/diff.counts" -v whyf="$WORK/diff.why" '
        # A value shaped like a credential, whatever key it sits under
        # (review 9cb9af67, finding 5): a long mixed-case base64 run; any
        # run of 32 or more token characters [A-Za-z0-9_-], which covers
        # lowercase hex and base64url of any case; a JWT segment; and a
        # bootstrap token (6.16 lowercase alphanumerics). The changed lines
        # come only from a four-class patch, whose values are short words,
        # numbers and paths, so these cost nothing legitimate.
        function tokenrun(s,   r, t) {
            t = s
            while (match(t, /[A-Za-z0-9+\/=]+/)) {
                r = substr(t, RSTART, RLENGTH)
                t = substr(t, RSTART + RLENGTH)
                if (length(r) >= 40 && r ~ /[A-Z]/ && r ~ /[a-z]/ && r ~ /[0-9]/) return 1
            }
            t = s
            while (match(t, /[A-Za-z0-9_-]+/)) {
                r = substr(t, RSTART, RLENGTH)
                t = substr(t, RSTART + RLENGTH)
                if (length(r) >= 32) return 1
            }
            if (s ~ /eyJ[A-Za-z0-9_-]*\./) return 1
            if (s ~ BOOTSTRAP) return 1
            return 0
        }
        # split a YAML line body into IND (indent), DASH ("- " or "") and
        # REST; KEY is the mapping key REST opens with, or "".
        function parse(body) {
            match(body, /^ */)
            IND = substr(body, 1, RLENGTH)
            REST = substr(body, RLENGTH + 1)
            DASH = ""
            if (REST ~ /^- /) { DASH = "- "; REST = substr(REST, 3) }
            KEY = ""
            if (REST ~ /^[A-Za-z0-9_.\/-]+:$/ || REST ~ /^[A-Za-z0-9_.\/-]+: /) {
                KEY = REST
                sub(/:.*$/, "", KEY)
            }
        }
        function secret(why) { print why > whyf; bad = 1 }
        BEGIN {
            plus = 0; minus = 0; bad = 0; odd = 0; hunk = 0
            # No interval expressions: mawk (Debian awk) does not read them.
            BOOTSTRAP = "[a-z0-9][a-z0-9][a-z0-9][a-z0-9][a-z0-9][a-z0-9]\\."
            for (i = 0; i < 16; i++) BOOTSTRAP = BOOTSTRAP "[a-z0-9]"
        }
        /^[[:space:]]*$/ { next }
        hunk == 0 && (/^--- / || /^\+\+\+ /) { print; next }
        /^@@ / { hunk = 1; print; next }
        /^No changes/ { next }
        /^[+-]/ {
            body = substr($0, 2)
            if (substr($0, 1, 1) == "+") plus++; else minus++
            parse(body)
            k = tolower(KEY)
            if (k == "ca" || k == "crt" || k == "secret" || k ~ /key$/ \
                || k ~ /token|secret|password|passphrase|privatekey|private_key|certificate|encryptionkey|aescbc|secretbox|bootstrap/) {
                secret("line " NR " of Talos\047s diff changes the value of \047" KEY "\047, a credential-shaped key")
                next
            }
            if (body ~ /BEGIN [A-Z ]*(PRIVATE KEY|CERTIFICATE)/ || tokenrun(body)) {
                secret("line " NR " of Talos\047s diff carries a credential-shaped value (a PEM header, a long token run, a JWT or a bootstrap token)")
                next
            }
            print
            next
        }
        /^ / {
            parse(substr($0, 2))
            if (KEY != "") {
                v = substr(REST, length(KEY) + 2)
                print " " IND DASH KEY ":" (v ~ /[^ ]/ ? " …" : "")
            } else if (REST == "") {
                print " " IND DASH
            } else {
                print " " IND DASH "…"
            }
            next
        }
        { odd++ }
        END {
            printf "%d %d %d\n", plus, minus, odd > cntf
            if (bad) exit 3
            if (odd) exit 4
        }' "$WORK/dry1.diff" > "$WORK/diff.rendered"
    case $? in
        0) ;;
        3) node_refuse "Talos's diff for $NODE touches a credential, so it is not rendered and nothing is applied: $(tr '\n' ' ' < "$WORK/diff.why")— a declaration here holds non-secret entries only (README.md beside the patches)" ;;
        4) node_fail "Talos's diff for $NODE held $(cut -d' ' -f3 "$WORK/diff.counts") line(s) that are not a unified diff, so it cannot be rendered safely; nothing was applied" ;;
        *) node_fail "the diff could not be rendered; nothing was applied" ;;
    esac
    read -r PLUS MINUS _ < "$WORK/diff.counts"
}

# findings <file> — check-declared.sh's findings as the plan and the
# failure line show them: each verdict with its key, never the values it
# printed beside them. The check prints live values — an UNDECLARED
# /var/local file's whole content, both sides of a DRIFT — and a signed
# plan lands on the packet's approve step, so they stay here (review
# 9cb9af67, finding 4). Its header and summary lines carry counts only.
findings() {
    awk '
        /^(MATCH|DRIFT|ABSENT|DOUBLED|UNDECLARED) / { printf "%-10s %s\n", $1, $2; next }
        /^read / || /^check-declared: / || /^UNVERIFIED / { print; next }
        { n++ }
        END { if (n) printf "(%d other line(s) of the check withheld)\n", n }' "$1" | mask
}

# has_summary <file> — the check's closing line for THIS node is there:
# `check-declared: <node>: <n> match, …`. Its absence means the check
# did not finish a comparison, whatever its exit code.
has_summary() {
    grep -Eq "^check-declared: $NODE: [0-9]+ match, " "$1"
}

# --- the plan -----------------------------------------------------------------
render() {
    local uid idem present
    read_back before
    [ "$GET_RC" -eq 0 ] \
        || node_fail "could not read $NODE's machine config through the talosconfig door ($(mask < "$WORK/before.get" | tr '\n' ' ')) — a node nobody read has no known state, so there is no plan"
    case "$CHECK_RC" in
        0 | 1)
            # Exit 0 or 1 is a verdict only beside the check's own summary
            # line: a check that died printing nothing would otherwise read
            # as "no findings", and the plan would say every entry is
            # ABSENT over a config nobody read (review be5ba8f7, finding 2).
            has_summary "$WORK/before.out" \
                || node_fail "check-declared.sh exited $CHECK_RC without its summary line, so it read nothing this plan could sign: $(mask < "$WORK/before.err" | tr '\n' ' ')" ;;
        2) node_refuse "check-declared.sh would not compare $NODE's live config with its declaration: $(mask < "$WORK/before.err" | tr '\n' ' ')" ;;
        *) node_fail "check-declared.sh could not read $NODE's live config (exit $CHECK_RC): $(mask < "$WORK/before.err" | tr '\n' ' ') — an effect nobody can read is not applied" ;;
    esac

    dry 1
    judge_dry 1
    render_diff
    [ $((PLUS + MINUS)) -gt 0 ] \
        || node_refuse "$NODE already carries the declaration at $SHA: Talos's dry run changes nothing, so there is nothing to converge (and an applied plan is never run twice)"

    # Idempotence, measured by Talos: the same patch applied twice.
    dry 2
    judge_dry 2
    if cmp -s "$WORK/dry1.summary" "$WORK/dry2.summary" && cmp -s "$WORK/dry1.diff" "$WORK/dry2.diff"; then
        idem="idempotent: yes — Talos's dry run of the patch applied twice is the dry run of it applied once"
    else
        # EVERY declared entry that is not ABSENT, in every class (review
        # 9cb9af67, finding 1): which part of the patch appends is not
        # visible from here, so any live entry could be the one doubled.
        present="$(grep -E '^(MATCH|DRIFT|DOUBLED) ' "$WORK/before.out" | awk '{ print $1 " " $2 }' | mask | tr '\n' ';')"
        # The live entries are not the fault and stay (finding 2): deleting
        # a working mount or mirror to unblock this would break the node,
        # and a de-duplicated DOUBLED entry reads MATCH and refuses the same.
        [ -z "$present" ] \
            || node_refuse "the patch is not idempotent on $NODE — Talos's dry run of it applied twice differs from once, because its merge appends list entries it cannot key — and these declared entries are already live: ${present%;} — so applying the whole declaration could add a second copy. Keep every live entry: they are working config, and deleting one to unblock this breaks the node (de-duplicating a DOUBLED entry leaves a MATCH, which is refused the same way). The remedy is a declaration narrowed to the entries that read ABSENT live on $NODE, or a partial patch that applies only those; nothing was applied"
        idem="idempotent: no — applied twice it would append again (Talos's merge appends list entries it cannot key); every entry it declares reads ABSENT live, so this run adds each once"
    fi

    uid="$(jq -r '.metadata.uid // "none"' "$WORK/node.json")"
    echo "plan: node-converge"
    echo "node: $NODE"
    echo "address: $NODE_ADDRESS"
    echo "estate role: $NODE_ROLE"
    echo "kubernetes node uid: $uid"
    echo "declaration: $REL at commit $SHA (blob $BLOB, the blob this checkout's HEAD carries)"
    echo "command: talosctl -n $NODE_ADDRESS patch machineconfig --mode=no-reboot --patch=<the declaration below> — Talos applies it live and refuses it if it would need a reboot. Reboot, upgrade, reset and etcd membership are never this verb."
    echo "read-back: talosctl -n $NODE_ADDRESS get machineconfig, through infra/cluster/talos/check-declared.sh $NODE against the declaration below, until every declared entry reads MATCH."
    echo
    echo "== the declaration ($(wc -l < "$WORK/decl/$NODE.yaml" | tr -d ' ') lines) =="
    cat "$WORK/decl/$NODE.yaml"
    echo
    echo "== $NODE against its declaration now (check-declared.sh, exit $CHECK_RC; verdicts and keys, never live values) =="
    findings "$WORK/before.out"
    echo
    echo "== Talos's dry run (--mode=no-reboot) =="
    cat "$WORK/dry1.summary"
    echo "$idem"
    echo "changed lines: +$PLUS -$MINUS"
    echo
    echo "== Talos's config diff (a context line shows its key only: a machine config's values include its keys and tokens) =="
    cat "$WORK/diff.rendered"
}

render > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"

if [ "$PLAN" -eq 1 ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write --------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    node_refuse "today's plan for $NODE (above) hashes to $HASH, not the approved plan $APPROVED — the node, its live config, Talos's diff or the declaration moved since the plan was signed (or this plan already ran). Nothing was applied; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "node-converge: plan $APPROVED still holds — applying $REL at $SHA to $NODE ($NODE_ADDRESS)"

# The apply is given a minute inside its container, as the door bounds
# every talosctl call (ops_talosctl); a dry run is given the door's default.
TA="$(BOSS_TALOS_TIMEOUT_S="${BOSS_NODE_CONVERGE_APPLY_TIMEOUT_S:-60}" ops_talosctl)"
# shellcheck disable=SC2086 # TA is one line the door prints to be word-split
if ! $TA -n "$NODE_ADDRESS" patch machineconfig --mode=no-reboot "--patch=$PATCH" < /dev/null > "$WORK/apply.out" 2> "$WORK/apply.err"; then
    mask < "$WORK/apply.err" | sed 's/^/    /' >&2
    if no_reboot_refusal "$WORK/apply.err"; then
        node_refuse "Talos refused to apply $REL to $NODE without a reboot (its words above) — nothing was applied, and a reboot is never this verb"
    fi
    node_fail "talosctl -n $NODE_ADDRESS patch machineconfig failed (its words above) — this run cannot say whether $NODE took the patch; read it with plan-a-node-converge before anything else"
fi
applied_at="$(date -u +%s)"
mask < "$WORK/apply.out" | sed 's/^/    /'
mask < "$WORK/apply.err" | sed 's/^/    /'
echo "node-converge: Talos accepted the patch on $NODE — reading it back (up to ${WAIT_S}s: the live machine config through check-declared.sh, then the Node Ready with its kubelet lease renewed at least ${SETTLE_S}s after the apply)"

# node_live — the Node as the cluster states it after the apply: READY
# (its Ready condition) and RENEW_S (the epoch of its kubelet lease's
# renewTime, kube-node-lease/<node>). WHY BOTH (review 9cb9af67, finding
# 3): every change these four classes make live restarts or reconfigures
# the kubelet or containerd, and Talos does not validate extraConfig
# against KubeletConfiguration — so a config that reads MATCH can sit
# under a kubelet that never came back. Ready alone can be stale: the
# Node keeps its last condition for the controller's grace period. The
# lease is renewed every ~10 s by a running kubelet only, so one renewed
# SETTLE_S after the apply is a kubelet running after the restart.
node_live() {
    READY="unread"
    RENEW="unread"
    RENEW_S=""
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if $K get node "$NODE" -o json --request-timeout=20s > "$WORK/live-node.json" 2> "$WORK/live-node.err"; then
        READY="$(jq -r '[(.status.conditions // [])[] | select(.type == "Ready") | .status] | .[0] // "none"' "$WORK/live-node.json" 2>/dev/null)"
    else
        READY="unread: $(mask < "$WORK/live-node.err" | tr '\n' ' ' | cut -c1-200)"
    fi
    # shellcheck disable=SC2086
    if $K get lease "$NODE" -n kube-node-lease -o json --request-timeout=20s > "$WORK/lease.json" 2> "$WORK/lease.err"; then
        RENEW="$(jq -r '.spec.renewTime // "none"' "$WORK/lease.json" 2>/dev/null)"
        case "${RENEW:-none}" in none) ;; *) RENEW_S="$(date -u -d "$RENEW" +%s 2>/dev/null)" ;; esac
    else
        RENEW="unread: $(mask < "$WORK/lease.err" | tr '\n' ' ' | cut -c1-200)"
    fi
}

# --- the read-back ------------------------------------------------------------
# THE VERDICT IS THE READ AT THE END OF THE BOUND (review be5ba8f7,
# finding 5). A change that restarts containerd but not the kubelet (a
# registry mirror) can report the node NotReady a status update after the
# kubelet's first fresh lease, so a node proven back early is read again:
# the loop runs until WAIT_S has passed, and only a read taken at or after
# it — config MATCH, Ready=True, lease fresh — prints the effect. Every
# successful run therefore takes the whole bound (120 s by default).
start="$(date -u +%s)"
said="not read yet"
seen_back=no
while :; do
    elapsed=$(($(date -u +%s) - start))
    read_back after
    good=no
    if [ "$GET_RC" -ne 0 ]; then
        said="the machine config did not read: $(mask < "$WORK/after.get" | tr '\n' ' ' | cut -c1-300)"
    elif [ "$CHECK_RC" -ne 0 ] || ! has_summary "$WORK/after.out"; then
        said="check-declared.sh exit $CHECK_RC: $(findings "$WORK/after.out" | tr '\n' ';' | cut -c1-600) $(mask < "$WORK/after.err" | tr '\n' ' ' | cut -c1-300)"
    else
        node_live
        case "${RENEW_S:-empty}" in empty | *[!0-9]*) fresh=no ;; *) [ "$RENEW_S" -ge $((applied_at + SETTLE_S)) ] && fresh=yes || fresh=no ;; esac
        if [ "$READY" = True ] && [ "$fresh" = yes ]; then
            good=yes
            seen_back=yes
            said="the Node was back (Ready=True, lease renewed $((RENEW_S - applied_at))s after the apply) and is read again at the end of the ${WAIT_S}s bound"
        elif [ "$seen_back" = yes ]; then
            said="the machine config reads every declared entry MATCH, and the Node was back and is NOT any more: Ready=$READY, kubelet lease renewTime $RENEW"
        else
            said="the machine config reads every declared entry MATCH, and the Node is NOT proven back: Ready=$READY, kubelet lease renewTime $RENEW (it must be at least ${SETTLE_S}s after the apply)"
        fi
    fi
    if [ "$elapsed" -ge "$WAIT_S" ]; then
        if [ "$good" = yes ]; then
            findings "$WORK/after.out"
            echo "node-converge: $NODE reads back its declaration at $SHA — check-declared.sh $NODE exits 0 against the live machine config, every declared entry MATCH, and Node $NODE Ready=True with its kubelet lease renewed $((RENEW_S - applied_at))s after the apply (after ${elapsed}s)"
            exit 0
        fi
        node_fail "Talos accepted the patch on $NODE, and after ${elapsed}s it is NOT proven to carry its declaration: $said. The patch stands — read it again with plan-a-node-converge $NODE $SHA"
    fi
    sleep "$POLL_S"
done
