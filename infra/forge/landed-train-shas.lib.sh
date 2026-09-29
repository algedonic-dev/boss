# landed-train-shas.lib.sh — ask the system of record which per-train CI
# images belong to a train that is DONE. Sourced, not executed, like its
# siblings prune-ci-images.lib.sh and prune-registry-tags.lib.sh.
#
#   landed_train_shas <curl-cmd> <jobs-url> <lookback> <log-prefix>
#
# Prints, on stdout, one SEVEN-HEX KEY per line — the first seven
# characters of every sha that belongs to a CLOSED `pr-train` packet and
# to no open one. Prints NOTHING when it cannot answer, and ALWAYS
# RETURNS 0.
#
#   train_sha_sets <curl-cmd> <jobs-url> <lookback> <log-prefix> <closed-out> <open-out>
#
# The SAME read, answered as its two halves rather than their
# difference: <closed-out> gets the keys CLOSED trains name (the two
# fields below), <open-out> every sha-shaped token an OPEN train
# carries. Returns 0 when the record answered, 1 when it could not —
# the reason in TRAIN_SETS_WHY, both files empty.
#
# TWO QUESTIONS, TWO MEANINGS (backlog 8d77d670, the adversarial review
# of the daily registry prune, M3). `landed_train_shas` answers the
# DISK SWEEP's question — which images are COLLECTABLE — so it
# subtracts every sha an open train mentions: closed minus open, the
# direction that collects less. prune-registry-versions.sh used that
# same answer as a KEEP set, where subtracting is the direction that
# keeps LESS: an open train's image, and a landed image an open train
# still names, were both deletable, unattended, every day. The prune
# now reads the halves and keeps closed UNION open. Each meaning is
# pinned where it is used: an_open_trains_sha_is_never_collected_as_landed
# (the sweep) and an_open_trains_sha_is_kept_by_the_prune (the prune),
# with the_train_lookup_answers_collectable_and_keep_from_one_read
# holding both side by side over one reply.
#
# WHY THIS EXISTS (backlog 9195a2a6). The forge's routine CI-image pass
# collected nothing for weeks and was not broken: measured 2026-09-11
# (ops-request 28d3599c) the system docker daemon held 13
# `boss-ci:<sha>` tags, 21.18GB with 18.58GB reclaimable, aged fifteen
# minutes to five hours — every one inside the pass's SIX-HOUR window, so
# the hourly pass correctly declined all of them. The window was sized
# when the forge ran 5-14 trains a day; the measured rate is now ~2.6 an
# hour, ~60 a day, which by itself keeps ~16 images in-window. That is
# the whole pile.
#
# AN AGE WINDOW IS A PROXY for "this image will not be needed again", and
# the proxy broke when the rate changed. The fact itself is in the
# record: a train's image is wanted until its train has LANDED or been
# ABANDONED, and a `pr-train` packet is CLOSED exactly then. Replacing
# the heuristic with the fact also self-tunes — it does not care whether
# the rate is 5 or 60 trains a day, which is precisely how the window
# failed.
#
# WHICH FIELDS, EXACTLY. Two, and only these two, because they are the
# shas a CI run BUILDS AN IMAGE FOR (`.forgejo/workflows/ci.yml` stamps
# `boss-ci:${{ github.sha }}`):
#   * the `assemble` step's `train_ref`, `train/<window>@<sha>` — the
#     train branch head, which is the sha the train's PR run builds;
#   * the `merged` step's `merge_ref` — the merge commit on main, which
#     is the sha the push-to-main run builds.
# Verified against the live SoR while this was written: all 13 images in
# the measurement above resolve, by these two fields, to closed trains.
# The `collect`/`assemble` steps also record CAR head shas (`car_heads`),
# and those are deliberately NOT read as terminal: no CI run builds an
# image for a car branch (the gate skips build-image), so admitting them
# would widen the deletion set for no reclaim at all.
#
# ---------------------------------------------------------------------
# EVERY FAILURE PATH PRUNES LESS, NEVER MORE
# ---------------------------------------------------------------------
# This is the one hard safety property, and it is why this function's
# output is an ADDITION to the age rule rather than a replacement for it.
# An empty answer leaves the prune loop behaving exactly as it did before
# this file existed. So:
#   * no `BOSS_JOBS_URL`          -> nothing, loudly. A read against a
#     guessed instance answers instead of erroring (two jobs APIs exist).
#   * curl fails, times out, 500s -> nothing, loudly.
#   * the reply does not parse    -> nothing, loudly.
#   * the reply lists NO trains   -> nothing, loudly, and deliberately
#     treated as a FAILED read rather than as "no train exists": an
#     unauthenticated read is not refused, it is given a smaller world
#     (measured: `total: 0` from raw curl where the signed read sees the
#     row). A mechanism that reads an empty page as fact would hand the
#     prune loop a set it cannot justify.
#   * a sha an OPEN train still references -> VETOED out of the answer,
#     even when a closed train names it too. The open reference wins
#     because that is the direction that keeps more.
# And the return code is ALWAYS 0: visibility is best-effort,
# destruction is not. The hourly sweep is what keeps the forge's disk
# above the floor CI boards through, and it must not fail — or stop
# working — because the system of record is down. Its sibling
# `prune_ci_images` returns non-zero when it cannot look at the DAEMON,
# which is the opposite case: blindness there would report success while
# freeing nothing, where blindness here only frees less.
#
# WHY A SEVEN-HEX KEY. `train_ref` carries the seven-character short sha
# and `merge_ref` twelve, while a CI tag is the full forty. Keying
# everything on the first seven is one comparison for all three lengths.
# An unrelated collision needs seven matching hex characters (1 in 2.7e8
# per pair, ~1e-5 a day at this rate of images and trains) and costs a
# 3.47GB LAN re-pull of an image the registry still holds — the same cost
# the age rule has always accepted when it guesses early.
#
# The read is SIGNED, as `automation:disk-floor-sweep`. See the
# empty-page note above: an unnamed read would not fail, it would
# silently retire the whole mechanism.
#
# THE MACHINE TOKEN RIDES IN A FILE, never in curl's argv, where every
# local user reads it in ps (backlog 5f3ad356; infra/lib/secret-header.sh).
# The file is made HERE, when this lib is sourced, because the disk sweep
# calls `landed_train_shas` inside $(…) and the helper makes its
# directory only in the script's own shell. So SOURCE THIS AFTER your own
# `trap … EXIT`: the helper's cleanup is chained in front of the trap
# that stands when it is made. A lib that cannot be read, or a file that
# cannot be made, leaves LTS_MT_HDR empty, and a token that is set but
# has no file is a read that cannot answer (prune less, never more).
# shellcheck source=infra/lib/secret-header.sh
if . "$(dirname "${BASH_SOURCE[0]}")/../lib/secret-header.sh" 2>/dev/null; then
    secret_header LTS_MT_HDR ${BOSS_MACHINE_TOKEN:+"x-boss-machine-token: $BOSS_MACHINE_TOKEN"} || LTS_MT_HDR=""
else
    LTS_MT_HDR=""
fi

train_sha_sets() {
    local curl_cmd="$1" jobs_url="$2" lookback="$3" prefix="$4" closed_f="$5" open_f="$6"
    local reply rc=0 trains closed_raw open_raw
    TRAIN_SETS_WHY=""
    : >"$closed_f" 2>/dev/null && : >"$open_f" 2>/dev/null || {
        TRAIN_SETS_WHY="the key files $closed_f / $open_f cannot be written"
        return 1
    }

    if [ -z "${jobs_url:-}" ]; then
        TRAIN_SETS_WHY="BOSS_JOBS_URL is not set and there is no safe default: two jobs APIs exist and a read against the wrong one answers 'no trains' instead of erroring"
        return 1
    fi
    if ! command -v jq >/dev/null 2>&1; then
        TRAIN_SETS_WHY="jq is not on this host, so the reply cannot be parsed"
        return 1
    fi

    # AN UNSIGNED READ SEES A SMALLER WORLD. Same header the ops-request
    # trigger sends, and the actor says automation in the audit trail.
    local actor="${BOSS_SWEEP_ACTOR:-automation:disk-floor-sweep}"
    local boss_user
    boss_user="{\"id\":\"$actor\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

    # --max-time, because the sweep is on a timer and a hung read must
    # not hold the disk remediation behind it. No retry: the next tick is
    # an hour away and the age window covers this pass either way.
    if [ -n "${BOSS_MACHINE_TOKEN:-}" ] && [ -z "${LTS_MT_HDR:-}" ]; then
        TRAIN_SETS_WHY="BOSS_MACHINE_TOKEN is set but its header file could not be made (infra/lib/secret-header.sh, beside this lib), and the token is never sent in curl's command line"
        return 1
    fi
    reply="$("$curl_cmd" -fsS --max-time 20 \
        -H "x-boss-user: $boss_user" \
        ${LTS_MT_HDR:+-H "$LTS_MT_HDR"} \
        "$jobs_url/api/jobs?kind=pr-train&limit=$lookback&full=true" 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ]; then
        TRAIN_SETS_WHY="reading $jobs_url failed (curl exit $rc): ${reply:-no reply}"
        return 1
    fi

    if ! trains="$(printf '%s' "$reply" | jq -r '(.data // []) | length' 2>/dev/null)" \
        || [ -z "$trains" ]; then
        TRAIN_SETS_WHY="the reply from $jobs_url is not a job listing this can read"
        return 1
    fi
    if [ "$trains" -eq 0 ]; then
        TRAIN_SETS_WHY="$jobs_url listed NO pr-train packets, which is a failed read and not a fact — an unauthenticated or out-of-scope read is handed an empty page rather than an error"
        return 1
    fi

    # A LIMIT IS NOT A FILTER: say how much of the record was read, so a
    # journal line cannot be mistaken for the whole truth. An image older
    # than this window resolves to no train and is left to the age rule.
    local total
    total="$(printf '%s' "$reply" | jq -r '.total // "?"' 2>/dev/null)"
    # STDOUT IS THE ANSWER, so every human-readable line goes to stderr:
    # the caller captures this function's stdout and a log line spliced
    # into it would be read as a sha.
    echo "$prefix: read $trains of ${total:-?} pr-train packets from $jobs_url (newest first)" >&2

    # The two fields, from CLOSED trains only. `train_ref` is
    # `train/<window>@<sha>`, so everything up to the last `@` goes.
    closed_raw="$(printf '%s' "$reply" | jq -r '
        (.data // [])[]
        | select(.status == "closed")
        | (.steps // [])[]
        | (.metadata // {})
        | (.train_ref? // empty), (.merge_ref? // empty)' 2>/dev/null)" || closed_raw=""

    # The veto set is deliberately BROAD: every sha-shaped token anywhere
    # in a train that is not closed. Over-matching here can only keep an
    # image, which is the safe direction.
    open_raw="$(printf '%s' "$reply" | jq -r '
        (.data // [])[] | select(.status != "closed") | tostring' 2>/dev/null)" || open_raw=""

    printf '%s\n' "$closed_raw" \
        | sed 's/.*@//' \
        | grep -oE '^[0-9a-f]{7,40}$' \
        | cut -c1-7 \
        | sort -u >"$closed_f"
    printf '%s\n' "$open_raw" \
        | grep -oE '[0-9a-f]{7,40}' \
        | cut -c1-7 \
        | sort -u >"$open_f"
    return 0
}

landed_train_shas() {
    local curl_cmd="$1" jobs_url="$2" lookback="$3" prefix="$4"
    local closed_f open_f

    _lts_fallback() { # $1 = why
        echo "$prefix: the landed-train lookup could not answer ($1), so this pass falls back to the AGE WINDOW alone — it prunes less, never more." >&2
    }

    closed_f="$(mktemp)" || { _lts_fallback "mktemp failed"; return 0; }
    open_f="$(mktemp)" || { rm -f "$closed_f"; _lts_fallback "mktemp failed"; return 0; }
    if ! train_sha_sets "$curl_cmd" "$jobs_url" "$lookback" "$prefix" "$closed_f" "$open_f"; then
        rm -f "$closed_f" "$open_f"
        _lts_fallback "$TRAIN_SETS_WHY"
        return 0
    fi

    local landed
    landed="$(comm -23 "$closed_f" "$open_f")"
    local n_closed n_open n_landed
    n_closed=$(grep -c . "$closed_f" || true)
    n_open=$(grep -c . "$open_f" || true)
    n_landed=$(printf '%s\n' "$landed" | grep -c . || true)
    rm -f "$closed_f" "$open_f"

    echo "$prefix: landed-train shas: $n_landed collectable ($n_closed from closed trains, $((n_closed - n_landed)) vetoed by the $n_open shas open trains still reference)" >&2
    [ -n "$landed" ] && printf '%s\n' "$landed"
    return 0
}
