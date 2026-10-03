#!/usr/bin/env bash
#
# break-glass-deposit — the cluster's half of delivering the break-glass
# kubeconfig to boss-gcp (backlog 7336cb5f). Runs as the
# boss-break-glass-deposit CronJob (infra/cluster/manifests/
# boss-break-glass-deposit.yaml), in the boss image, as uid 1500.
#
# WHY A CLUSTER JOB, AND WHY PUSH. boss-gcp is the only estate node that
# is neither the forge nor inside the cluster, so it is the road back
# when the forge is down — and it cannot read a k8s Secret without the
# very credential being delivered. So the credential is pushed to it
# over a deposit-only forced-command key, the transport boss-backup
# shipped the nightly dump over until 2026-10-01 (design 835c0c9c, "transport"),
# by a Job that reads ONE Secret by name and places nothing by hand.
#
# ONE PASS:
#   1. THE DEPOSIT KEY. Mounted from Secret break-glass-deposit-key at
#      $BOSS_DEPOSIT_KEY_DIR, group-readable (0440, root:<fsGroup>). A
#      Secret with no key yet is MINTED here — ssh-keygen in this pod,
#      the private half patched into that one Secret (the Role names it)
#      and never printed — and the pass stops after verifying, printing
#      the authorized_keys line David places on boss-gcp: authorizing a
#      key is his signed act, not a Job's. A Secret that already holds a
#      key the mount does not show yet is left alone.
#   2. THE KUBECONFIG, assembled in memory-backed scratch from Secret
#      break-glass-operator-token (token + ca.crt) and the address
#      boss-gcp dials, $BOSS_DEPOSIT_API_SERVER.
#   3. VERIFY BY EFFECT, IN BOTH DIRECTIONS, WITH THAT FILE (design
#      835c0c9c, "verify-the-negative": refuse the deposit if the
#      credential can do what it must not). `get nodes` must be YES;
#      `get secrets`, `delete pods`, `create deployments` in the boss
#      namespace must each be NO; an image-only roll of both the main
#      and init container must be ACCEPTED; and three server-side DRY-RUN patches
#      of the $BOSS_DEPOSIT_PROBE_DEPLOYMENT Deployment must each be
#      REFUSED — its command (design b08725c2, Decided: RBAC scopes
#      `patch` to an object, never a field, so a pod-template write
#      reads every credential that template names), a FOREIGN image and
#      its metadata.ownerReferences (review 89c716e0 of 4e1c33b4,
#      findings B1 and N1). 4e1c33b4 narrows the grant; this is what
#      notices whether it did. Each refusal must name its own policy
#      rule: a CEL error or a policy denying everything proves nothing.
#      The allowed roll uses a different synthetic tag in the observed
#      repository. It proves admission, never a pull, boot or rollback
#      artifact; no dry-run patch persists. An answer that is neither the yes
#      nor the no asked for — a TLS error, a timeout, a NotFound — is
#      UNMEASURED, and unmeasured refuses exactly like a wrong answer:
#      no evidence is not a pass.
#   4. SHIP: the kubeconfig on ssh's stdin to the forced-command receiver
#      (infra/gcp/ops-credential-recv.sh), host key pinned from
#      $BOSS_DEPOSIT_KNOWN_HOSTS, and the receiver's byte count READ BACK
#      against the bytes sent.
#
# NON-ROOT, AND WHY THAT WORKS WITH A MOUNTED KEY (design b08725c2 Q3,
# David 2026-09-29). OpenSSH refuses a private key whose mode admits
# group or other ONLY when the running user owns the file
# (sshkey_perm_ok: `st.st_uid == getuid() && (st.st_mode & 077) != 0`).
# The kubelet projects a Secret volume as root and, with fsGroup, gives
# it that group — so uid 1500 reads a root:1500 0440 key through its
# group and ssh does not apply the check. boss-backup's retired 0400 ship key "only
# ever succeeded because it runs as root" for the opposite reason: root
# owns the file, so the check applies and 0400 passes it. Rehearsed with
# a real sshd and the receiver before this landed (the car's report).
#
# NOTHING SECRET IS PRINTED OR PASSED IN AN ARGV: the token, the CA and
# the private key reach files through printf (a builtin) or kubectl's
# --patch-file, and every message names paths, byte counts and verdicts.
#
# AN ACCEPTED PATCH IS A REFUSAL, WHATEVER THE BINDING SAYS (backlog
# e4a9a9b3, the Deny car; review 0d3019f0 F1). While row B's policy
# only reported, [Warn, Audit], an accepted patch read `not yet` and
# exited 75 (backlog 17a7bd18). The Deny car deleted that branch: after
# it, a binding DELETED — the car's own named misfire rollback, which a
# revert car makes permanent — or moved back to Warn or Audit makes the
# same three patches land again, and by then boss-gcp may already hold
# a kubeconfig that can make them. A quiet `not yet` there is silence
# about a widened credential. So any accepted must-refuse patch refuses
# on a RED line, and the pass still reads the live binding with its OWN
# account (never the credential under test) — not to excuse the patch,
# but so the line says which it was: Deny that does not refuse, a
# binding moved off Deny (naming it and its actions), one that binds
# another policy, one that is absent, or one that could not be read.
#
# EVERY REFUSAL REACHES A READER (review b6d2a716, N1). A refusal
# prints ONE `RED break-glass-deposit refused: <why>` line, and a key
# boss-gcp does not hold yet prints `RED break-glass-deposit/
# authorize-key authorize: …` with the authorized_keys line and the
# pinned host key's fingerprint to check first. boss-chore.sh keeps
# every RED line whole, and file-backlog-items-on-break-glass-deposit-
# red files each route as a backlog-item owned by the platform owner.
#
# Exit: 0 deposited (or unchanged); 1 refused or failed, named — a
# failed Job is the alarm; 78 a refusal of how it was invoked. There is
# no not-yet: nothing this pass measures is changed by waiting.
set -euo pipefail

ME=break-glass-deposit
NS="${BOSS_DEPOSIT_NAMESPACE:-boss}"
TOKEN_SECRET="${BOSS_DEPOSIT_TOKEN_SECRET:-break-glass-operator-token}"
KEY_SECRET="${BOSS_DEPOSIT_KEY_SECRET:-break-glass-deposit-key}"
KEY_DIR="${BOSS_DEPOSIT_KEY_DIR:-/keys}"
KNOWN_HOSTS="${BOSS_DEPOSIT_KNOWN_HOSTS:-/known/known_hosts}"
SERVER="${BOSS_DEPOSIT_API_SERVER:-}"
TARGET="${BOSS_DEPOSIT_TARGET:-}"
PROBE_DEPLOYMENT="${BOSS_DEPOSIT_PROBE_DEPLOYMENT:-boss}"
# Row B's binding and the policy it must bind
# (infra/cluster/manifests/boss-break-glass-operator.yaml); the
# ClusterRole in this Job's manifest names the same binding, and
# break_glass_deposit_sh.rs holds all of them equal.
POLICY_BINDING="${BOSS_DEPOSIT_POLICY_BINDING:-break-glass-operator-image-only}"
POLICY=break-glass-operator-image-only
# The route every RED line carries — the chore judge's dedup key.
ROUTE=break-glass-deposit
# The one receiver path, spelled as the installer spells it — the test
# holds this line and install-ops-credential-receiver.sh's equal.
FORCED='command="exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig",restrict'

# red ROUTE KIND TEXT — the one line shape boss-chore.sh keeps whole and
# maintenance.chore.file_reds files from: `RED <route> <kind>: <text>`.
red() { echo "RED $1 $2: $3" >&2; }
usage() { red "$ROUTE" misconfigured "$*"; exit 78; }
refuse() { red "$ROUTE" refused "$*"; exit 1; }
say() { echo "$ME: $*"; }
# stop TEXT — not deposited, and the reason is already on a RED line
# (the key to authorize), so this adds no second item beside it.
stop() { echo "$ME: not deposited — $*" >&2; exit 1; }

[[ $SERVER =~ ^https://[A-Za-z0-9.:-]+$ ]] \
    || usage "BOSS_DEPOSIT_API_SERVER='$SERVER' is not the https:// address boss-gcp dials (the manifest sets it)"
[[ $TARGET =~ ^[a-z_][a-z0-9_-]*@[A-Za-z0-9.:-]+$ ]] \
    || usage "BOSS_DEPOSIT_TARGET='$TARGET' is not <account>@<host> (the manifest sets it)"

umask 077
WORK="$(mktemp -d "${TMPDIR:-/tmp}/deposit.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
ERR="$WORK/err"

# One line of kubectl's own words, for a record: never a value, since
# no value is ever handed to kubectl on its command line.
why() { tr '\n' ' ' < "$ERR" | cut -c1-300; }

# authorize PUBFILE — the one act David owes this delivery, on a RED
# line of its own so it reaches him as an open item rather than a
# closed packet's output (review b6d2a716, N1). It carries the pinned
# host key's fingerprint, read from the file the ssh leg trusts, because
# placing the line trusts that key (N8): compare it on the host first.
authorize() {
    local fp
    fp="$(ssh-keygen -l -f "$KNOWN_HOSTS" 2>/dev/null | awk 'NR == 1 { print $2 }')" || fp=""
    red "$ROUTE/authorize-key" authorize "on boss-gcp itself, run ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub and compare it with the host key this Job pins, ${fp:-UNREAD (no pinned key at $KNOWN_HOSTS)}; only if they match, place this line in ~${TARGET%@*}/.ssh/authorized_keys there: $FORCED $(cat "$1")"
}

# --- 1. the deposit key -------------------------------------------------
KEY="$KEY_DIR/id_ed25519"
PUB="$KEY_DIR/id_ed25519.pub"
MINTED=0
if [ ! -s "$KEY" ]; then
    if ! kubectl -n "$NS" get secret "$KEY_SECRET" -o json > "$WORK/key.json" 2>"$ERR"; then
        refuse "Secret $NS/$KEY_SECRET could not be read — the manifest declares it empty and this Job mints into it: $(why)"
    fi
    held="$(jq -r '.data["id_ed25519"] // empty' "$WORK/key.json")"
    if [ -n "$held" ]; then
        jq -r '.data["id_ed25519.pub"] // empty' "$WORK/key.json" | base64 -d > "$WORK/key.pub" 2>/dev/null || :
        if [ -s "$WORK/key.pub" ]; then
            authorize "$WORK/key.pub"
            stop "Secret $NS/$KEY_SECRET already holds a deposit key this pod's mount does not show (minted after the pod started); nothing minted — the next run deposits with it, once boss-gcp holds the line above"
        fi
        refuse "Secret $NS/$KEY_SECRET already holds a deposit key this pod's mount does not show, and no public half beside it; nothing minted, nothing deposited"
    fi
    rv="$(jq -r '.metadata.resourceVersion // empty' "$WORK/key.json")"
    [ -n "$rv" ] || refuse "Secret $NS/$KEY_SECRET carries no resourceVersion; not minting over an object this pass cannot pin"
    ssh-keygen -q -t ed25519 -N '' -C "break-glass-deposit@$NS" -f "$WORK/key" >/dev/null 2>"$ERR" \
        || refuse "ssh-keygen could not mint the deposit key: $(why)"
    # The patch TESTS the resourceVersion read above, so a key another
    # run minted in between is never overwritten — it may already be the
    # one boss-gcp authorizes.
    jq -n --arg rv "$rv" --rawfile k "$WORK/key" --rawfile p "$WORK/key.pub" '[
        {op: "test", path: "/metadata/resourceVersion", value: $rv},
        {op: "add", path: "/data", value: {"id_ed25519": ($k | @base64), "id_ed25519.pub": ($p | @base64)}}
    ]' > "$WORK/key.patch"
    if ! kubectl -n "$NS" patch secret "$KEY_SECRET" --type=json --patch-file "$WORK/key.patch" >/dev/null 2>"$ERR"; then
        refuse "the minted key could not be written into Secret $NS/$KEY_SECRET (nothing else holds it; the next run mints again): $(why)"
    fi
    cp "$WORK/key.pub" "$WORK/minted.pub"
    MINTED=1
    say "minted a deposit key into Secret $NS/$KEY_SECRET; the private half never left this pod"
    authorize "$WORK/minted.pub"
fi

# --- 2. the kubeconfig --------------------------------------------------
if ! kubectl -n "$NS" get secret "$TOKEN_SECRET" -o json > "$WORK/token.json" 2>"$ERR"; then
    refuse "Secret $NS/$TOKEN_SECRET could not be read: $(why)"
fi
TOKEN="$(jq -r '.data.token // empty' "$WORK/token.json" | base64 -d 2>/dev/null || :)"
CA="$(jq -r '.data["ca.crt"] // empty' "$WORK/token.json")"
rm -f "$WORK/token.json"
[[ $TOKEN =~ ^[A-Za-z0-9._-]{20,}$ ]] \
    || refuse "Secret $NS/$TOKEN_SECRET holds no ServiceAccount token (the token controller fills it a moment after the Secret is created)"
[[ $CA =~ ^[A-Za-z0-9+/=]{20,}$ ]] \
    || refuse "Secret $NS/$TOKEN_SECRET carries no ca.crt"
KC="$WORK/kubeconfig"
printf '%s\n' \
    "apiVersion: v1" \
    "kind: Config" \
    "clusters:" \
    "  - name: break-glass" \
    "    cluster:" \
    "      server: $SERVER" \
    "      certificate-authority-data: $CA" \
    "users:" \
    "  - name: break-glass-operator" \
    "    user:" \
    "      token: $TOKEN" \
    "contexts:" \
    "  - name: break-glass" \
    "    context:" \
    "      cluster: break-glass" \
    "      user: break-glass-operator" \
    "      namespace: $NS" \
    "current-context: break-glass" > "$KC"
BYTES="$(stat -c %s "$KC")"
say "assembled a kubeconfig for $SERVER from Secret $NS/$TOKEN_SECRET ($BYTES bytes)"

# --- 3. verify by effect, both directions -------------------------------
K=(kubectl --kubeconfig="$KC" --request-timeout=20s)
FAILED=""
# The must-refuse patches that were ACCEPTED — each one a refusal of
# the deposit; the binding read below says why it landed.
ACCEPTED=()
# can_i WANT ARGS… — `auth can-i` prints exactly `yes` or `no`; anything
# else on stdout, or nothing, is an answer this pass did not get.
can_i() {
    local want="$1" got
    shift
    got="$("${K[@]}" auth can-i "$@" 2>"$ERR" || :)"
    if [ "$got" = "$want" ]; then
        say "  can-i $*: $got (wanted $want)"
    elif [ "$got" = yes ] || [ "$got" = no ]; then
        say "  can-i $*: $got (wanted $want) — WRONG"
        FAILED="${FAILED:+$FAILED; }can-i $* answered $got"
    else
        say "  can-i $*: UNMEASURED — $(why)"
        FAILED="${FAILED:+$FAILED; }can-i $* unmeasured"
    fi
}
say "verifying the kubeconfig by effect, with the file itself:"
can_i yes get nodes
can_i no get secrets -n "$NS"
can_i no delete pods -n "$NS"
can_i no create deployments -n "$NS"
# must_accept — prove that admission allows a two-image roll, using the
# credential under test to observe the live pair first. This probe does
# not need a registry artifact: the server judges admission, not pulling
# or booting the synthetic tag. An ambiguous observation is no evidence.
must_accept() {
    local patch
    if "${K[@]}" -n "$NS" get deployment "$PROBE_DEPLOYMENT" -o json > "$WORK/deployment.json" 2>"$ERR" \
        && patch="$(jq -ce '
            def repository:
                if type != "string" then error("image is not a string") else
                    "^(?<repository>[a-z0-9][a-z0-9.-]*(?::[0-9]+)?(?:/[a-z0-9]+(?:[._-][a-z0-9]+)*)+)(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}(?:@sha256:[a-f0-9]{64})?|@sha256:[a-f0-9]{64})$" as $pattern |
                    if test($pattern) then capture($pattern).repository else error("malformed image reference") end
                end;
            .spec.template.spec as $pod |
            if ($pod.containers | type) != "array" or ($pod.containers | length) != 1 or
               ($pod.initContainers | type) != "array" or ($pod.initContainers | length) != 1
            then error("expected one main and one init container") else
                $pod.containers[0].image as $main | $pod.initContainers[0].image as $init |
                ($main | repository) as $repository |
                if ($init | repository) != $repository then error("image repositories disagree") else
                    ($repository + ":break-glass-deposit-probe") as $candidate |
                    ([$candidate, $candidate + "-alternate", $candidate + "-alternate-2"] |
                     map(select(. != $main and . != $init)) | .[0]) as $image |
                    [{op:"replace",path:"/spec/template/spec/containers/0/image",value:$image},
                     {op:"replace",path:"/spec/template/spec/initContainers/0/image",value:$image}]
                end
            end
        ' "$WORK/deployment.json" 2>"$ERR")" \
        && "${K[@]}" -n "$NS" patch deployment "$PROBE_DEPLOYMENT" --dry-run=server --type=json -p "$patch" >/dev/null 2>"$ERR"; then
        say "  patch deployment/$PROBE_DEPLOYMENT allowed image roll (server dry-run): accepted (wanted accepted; admission only)"
    else
        say "  patch deployment/$PROBE_DEPLOYMENT allowed image roll (server dry-run): UNMEASURED — $(why)"
        FAILED="${FAILED:+$FAILED; }the allowed image roll was not proven"
    fi
}
must_accept
# must_refuse LABEL WHY MESSAGE PATCH — a write the credential must NOT be able
# to make, sent as a server-side DRY RUN: authorization and admission
# judge it exactly as a real patch, and nothing is persisted. Refused
# means kubectl failed AND the named policy and binding denied the
# request with this rule's canonical message; accepted — a Warn-mode policy's warning included —
# is the credential doing what it must not; any other failure (NotFound,
# a timeout) is unmeasured. Each PATCH uses `add`, never `replace`, so a
# field the object does not carry yet is still a change to judge rather
# than a JSON-patch error mistaken for a refusal.
must_refuse() {
    local label="$1" reach="$2" message="$3" patch="$4"
    if "${K[@]}" -n "$NS" patch deployment "$PROBE_DEPLOYMENT" --dry-run=server --type=json -p "$patch" >/dev/null 2>"$ERR"; then
        say "  patch deployment/$PROBE_DEPLOYMENT $label (server dry-run): ACCEPTED — $reach (4e1c33b4)"
        ACCEPTED+=("$label")
    elif grep -Fq "ValidatingAdmissionPolicy '$POLICY' with binding '$POLICY_BINDING' denied request: $message" "$ERR"; then
        # This is the durable maintenance receipt's admission evidence.
        # Copy the observed refusal whole: the policy's long messages
        # otherwise outlast why()'s diagnostic excerpt (d40eddc0).
        say "  patch deployment/$PROBE_DEPLOYMENT $label (server dry-run): refused (wanted refused) — $(tr '\n' ' ' < "$ERR")"
    else
        say "  patch deployment/$PROBE_DEPLOYMENT $label (server dry-run): UNMEASURED — $(why)"
        FAILED="${FAILED:+$FAILED; }the $label patch probe unmeasured"
    fi
}
# The pod-template command (design b08725c2, Decided).
must_refuse "pod-template command" \
    "this credential can rewrite what a pod runs, and so read every Secret that template names through pods/log" \
    'break-glass-operator may change only an image; this patch changes a container command, args, env or other field, or adds or removes a container' \
    '[{"op":"add","path":"/spec/template/spec/containers/0/command","value":["/bin/false"]}]'
# A FOREIGN image (review 89c716e0 of 4e1c33b4, finding B1). The boss
# container declares no command, so an image from any registry runs its
# own entrypoint with every env credential the template names, and
# pods/log hands them back. The rollback this credential exists for sets
# a NAMED build from the forge registry; an image from anywhere else is
# the credential read, not the rollback.
must_refuse "foreign image" \
    "this credential can run any image with this template's credentials, and read them back through pods/log" \
    'break-glass-operator may roll only to a tag or digest in the repository the workload already pulls from; a foreign image runs its own entrypoint with every credential the template names' \
    '[{"op":"add","path":"/spec/template/spec/containers/0/image","value":"docker.io/library/busybox:1.36"}]'
# ownerReferences (the same review, finding N1): an owner that does not
# exist hands the workload to the garbage collector, which deletes it —
# a removal by a credential that holds no `delete`.
must_refuse "metadata.ownerReferences" \
    "this credential can hand the workload to the garbage collector, which deletes it" \
    'break-glass-operator may not change a workload ownerReferences (an owner that does not exist hands the workload to the garbage collector, which deletes it)' \
    '[{"op":"add","path":"/metadata/ownerReferences","value":[{"apiVersion":"v1","kind":"ConfigMap","name":"break-glass-deposit-probe","uid":"00000000-0000-0000-0000-000000000000"}]}]'
ACCEPTED_TEXT=""
for label in "${ACCEPTED[@]}"; do
    ACCEPTED_TEXT="${ACCEPTED_TEXT:+$ACCEPTED_TEXT; }a $label patch was accepted"
done

# --- 3b. what the binding says, when a must-refuse patch landed --------
# Never an excuse — see the header — only the reason, on the RED line.
BINDING_READ=""
if [ "${#ACCEPTED[@]}" -gt 0 ]; then
    # With this Job's OWN account — the ClusterRole in its manifest names
    # the binding and grants get alone — never with the credential under
    # test, whose answers are what is being judged.
    if kubectl get validatingadmissionpolicybinding "$POLICY_BINDING" -o json > "$WORK/binding.json" 2>"$ERR"; then
        actions="$(jq -r '(.spec.validationActions // []) | join(",")' "$WORK/binding.json" 2>/dev/null)" || actions=""
        policy="$(jq -r '.spec.policyName // empty' "$WORK/binding.json" 2>/dev/null)" || policy=""
        case ",$actions," in
            *,Deny,*)
                BINDING_READ="though binding $POLICY_BINDING denies (validationActions: $actions), the policy does not refuse what it was written to" ;;
            *)
                BINDING_READ="binding $POLICY_BINDING does not deny (validationActions: ${actions:-none}): moved off Deny by hand or by a revert, or the converge has not applied the tree's Deny" ;;
        esac
        [ "$policy" = "$POLICY" ] \
            || BINDING_READ="$BINDING_READ, and it binds policy ${policy:-none}, not $POLICY"
    elif grep -q 'NotFound' "$ERR"; then
        BINDING_READ="binding $POLICY_BINDING is absent: deleted (the Deny car's named misfire rollback) or reverted, so nothing judges the break-glass patch; the next converge applies it again unless the tree no longer declares it"
    else
        BINDING_READ="binding $POLICY_BINDING could not be read ($(why))"
    fi
    BINDING_READ="$BINDING_READ — and any copy of this credential boss-gcp already holds can make the accepted patches until it denies again"
fi
if [ -n "$FAILED" ] || [ -n "$ACCEPTED_TEXT" ]; then
    refuse "the credential is not the one declared, so it is NOT deposited: $FAILED${FAILED:+${ACCEPTED_TEXT:+; }}$ACCEPTED_TEXT${BINDING_READ:+; $BINDING_READ}"
fi
say "the credential can read the cluster, admission allows an image roll, and it can do none of the six things it must not"

if [ "$MINTED" = 1 ]; then
    stop "the deposit key was minted on this run, so boss-gcp cannot authorize it yet; place the line above, and a run after that deposits"
fi

# --- 4. ship --------------------------------------------------------------
[ -s "$KNOWN_HOSTS" ] || refuse "no pinned host key at $KNOWN_HOSTS — a credential is never shipped to a host this pass cannot authenticate"
# -T: no pty — the receiver reads bytes, and a pty would translate them.
SSH_OPTS=(-F /dev/null -T -i "$KEY" -o IdentitiesOnly=yes -o BatchMode=yes
    -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$KNOWN_HOSTS" -o GlobalKnownHostsFile=/dev/null
    -o ConnectTimeout=20 -o ServerAliveInterval=10 -o ServerAliveCountMax=3)
src=0
ssh "${SSH_OPTS[@]}" "$TARGET" < "$KC" > "$WORK/recv.out" 2>"$ERR" || src=$?
sed "s/^/  $TARGET: /" "$WORK/recv.out"
if [ "$src" -ne 0 ]; then
    if grep -q 'Permission denied' "$ERR" && [ -s "$PUB" ]; then
        authorize "$PUB"
        stop "boss-gcp does not accept this deposit key yet (ssh exit $src): $(why)"
    fi
    refuse "the deposit to $TARGET failed (ssh exit $src): $(why)"
fi
got="$(sed -n 's/^ops-credential-recv: kubeconfig: received \([0-9][0-9]*\) bytes;.*/\1/p' "$WORK/recv.out")"
[ "$got" = "$BYTES" ] \
    || refuse "the receiver reported '${got:-no byte count}' where $BYTES bytes were sent — the deposit is not proven"
say "deposited: $TARGET received the $BYTES bytes sent"
