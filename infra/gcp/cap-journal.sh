#!/usr/bin/env bash
# cap-journal.sh — put boss-gcp's journald bound down, and keep it down.
#
# WHY (backlog d3c7eada, 2026-09-29). The signed reclaim-gcp-root
# (ops-request 46cc2304) vacuumed boss-gcp's journal from 3.9G to 916M
# and said in its own effect line that "the journal grows back without a
# SystemMaxUse bound" — journald's default ceiling, 10% of the root up to
# 4G, is where it sat. A vacuum is a loan; this is the bound. The value
# and its reasoning live in infra/gcp/journald-cap.conf, not here.
#
# Run by boss-gcp-converge on every tick, so it is idempotent: an
# identical drop-in is left alone and journald is NOT restarted; a missing
# or hand-edited one is written from the tree and journald is restarted,
# which is how it re-reads its config and trims to the lower ceiling.
# A failure exits non-zero naming what failed — a cap that silently did
# not land is the loan again.
#
# Driven by crates/core/boss-testing/tests/cap_journal_sh.rs. The two
# env knobs exist for it (and for the converge lint); on the host both
# are the defaults.
set -euo pipefail

SRC="$(cd "$(dirname "$0")" && pwd)/journald-cap.conf"
DIR="${BOSS_JOURNALD_CONF_DIR:-/etc/systemd/journald.conf.d}"
SYSTEMCTL="${BOSS_JOURNALD_SYSTEMCTL:-systemctl}"
DEST="$DIR/boss-gcp-journal-cap.conf"

[ -r "$SRC" ] || { echo "cap-journal: $SRC is missing — nothing to install" >&2; exit 1; }
bound=$(grep -E '^SystemMaxUse=' "$SRC" || true)
[ -n "$bound" ] || { echo "cap-journal: $SRC declares no SystemMaxUse" >&2; exit 1; }

if [ -f "$DEST" ] && cmp -s "$SRC" "$DEST"; then
    echo "cap-journal: $bound unchanged at $DEST"
    exit 0
fi

mkdir -p "$DIR"
install -m 0644 "$SRC" "$DEST"
if ! "$SYSTEMCTL" restart systemd-journald; then
    # The file comes back OUT, so the next tick sees a difference and
    # retries the restart — left in place, it would read "unchanged" for
    # ever over a journald still running on its old ceiling.
    rm -f "$DEST"
    echo "cap-journal: restarting systemd-journald FAILED after writing $bound —" >&2
    echo "    $DEST removed again so the next tick retries; journald keeps its old" >&2
    echo "    ceiling until then." >&2
    exit 1
fi
echo "cap-journal: installed $bound at $DEST and restarted systemd-journald"
