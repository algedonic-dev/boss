#!/usr/bin/env bash
# 67e08c6a: the accepted host tree owns this fixed local credential door.
set -euo pipefail
. "$(dirname "$0")/../lib/host-check.sh"
# The fixed child has no path overrides: these writes are always on this host.
host_path bridge-private /etc/boss-bridge /etc/boss-bridge
host_path bridge-state /var/lib/boss-bridge /var/lib/boss-bridge
host_path bridge-release /opt/boss-bridge /opt/boss-bridge
host_path bridge-unit /etc/systemd/system /etc/systemd/system
host_command /usr/bin/systemctl
host_command /usr/bin/apt-get
host_command /usr/sbin/useradd
host_check boss-gcp proton-bridge
if [[ $# == 1 && $1 == capture ]]; then
    # The child names its own refusal on stderr from an authored allowlist.
    # Forward that once, as the inherited stream did; the generic line is for
    # a child that died without a word (review 7301d10f: every refusal used
    # to read twice, the child's reason and then this line under it).
    said=$(mktemp) || { echo 'Bridge operation UNAVAILABLE; credential file outcome unconfirmed' >&2; exit 75; }
    trap 'rm -f "$said"' EXIT
    status=0
    report=$(/usr/bin/python3 "$(dirname "$0")/proton_bridge.py" capture 2>"$said") || status=$?
    if [[ $status != 0 ]]; then
        if [[ -s $said ]]; then cat "$said" >&2; else echo 'Bridge operation UNAVAILABLE; credential file outcome unconfirmed' >&2; fi
        exit 75
    fi
    cat "$said" >&2
    # Do not echo a malformed report: even diagnostics must carry no secret.
    pattern='^path=/etc/boss-bridge/adapter-credential[.]json bytes=([1-9][0-9]{0,19})$'
    if [[ ! $report =~ $pattern ]]; then
        echo 'Bridge capture report UNAVAILABLE; credential file outcome unconfirmed' >&2
        exit 75
    fi
    printf 'captured local Bridge credential: path=/etc/boss-bridge/adapter-credential.json bytes=%s; real endpoint verification still owed\n' "${BASH_REMATCH[1]}"
    exit 0
fi
exec /usr/bin/python3 "$(dirname "$0")/proton_bridge.py" "$@"
