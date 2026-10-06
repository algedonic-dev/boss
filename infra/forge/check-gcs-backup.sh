#!/usr/bin/env bash
# Read one existing Secret, then the newest remote archive's generation.
# No argument names a credential, bucket, object, command, or local file.
# The weekly ops-request owns the output and exit status (backlog 3a68ab36).
set -euo pipefail
umask 077
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"
fail() { echo "check-gcs-backup: FAILED — $*" >&2; exit 1; }
[ $# -eq 0 ] || fail 'this read-only check accepts no arguments'
[ "$(stat -f -c %T /dev/shm)" = tmpfs ] || fail '/dev/shm is not tmpfs; no credential was read'
WORK="$(mktemp -d /dev/shm/boss-gcs-readback.XXXXXXXX)"
CONTAINER=""
cleanup() {
    # Killing a host-side docker client need not kill its container. Remove
    # only this run's generated name before releasing the private Secret.
    if [ -n "$CONTAINER" ]; then
        timeout -k 5 20 sudo -n docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
ops_image_ready > "$WORK/door.log" 2>&1 || fail 'the kubectl door image could not be made ready'
K="$(ops_kubectl)"
# This is the only Kubernetes read. Never echo provider error bodies or the
# Secret response; those bytes belong only to this private tmpfs directory.
# shellcheck disable=SC2086 # the existing door returns a word-split command
timeout -k 5 60 $K get secret boss-gcs-offsite -n boss -o json --request-timeout=30s \
    > "$WORK/secret.json" 2> "$WORK/secret.err" || fail 'cannot read Secret boss/boss-gcs-offsite'
jq_doc_file "$WORK/secret.json" \
    && jq -e -s 'length == 1 and (.[0] | .kind == "Secret" and .metadata.name == "boss-gcs-offsite"
    and .metadata.namespace == "boss" and (.data["sa.json"] | type == "string" and length > 0)
    and (.data.bucket | type == "string" and length > 0))' "$WORK/secret.json" >/dev/null \
    || fail 'Secret boss/boss-gcs-offsite did not carry both required keys'
jq -r '.data["sa.json"]' "$WORK/secret.json" | base64 -d > "$WORK/sa.json" \
    || fail 'the existing service-account key is not base64'
jq -r '.data.bucket' "$WORK/secret.json" | base64 -d > "$WORK/bucket" \
    || fail 'the existing bucket name is not base64'
# The same SDK image the nightly uploader already uses. The container's
# credential cache lives only in its bounded tmpfs; the archive streams through
# bounded validators without a workspace copy. The container has no
# database, recovery-kit or backup-volume mount and no Kubernetes credential.
if ! timeout -k 5 120 sudo -n docker image inspect google/cloud-sdk:alpine >/dev/null 2>&1; then
    timeout -k 5 120 sudo -n docker pull google/cloud-sdk:alpine > "$WORK/pull.log" 2>&1 \
        || fail 'the existing uploader SDK image could not be pulled within 120 seconds'
fi
rc=0
CONTAINER="boss-gcs-readback-${WORK##*.}"
timeout -k 5 900 sudo -n docker run --rm --name "$CONTAINER" --read-only --memory=2g --pids-limit=128 \
    --cap-drop=ALL --security-opt=no-new-privileges \
    --tmpfs /work:rw,nosuid,nodev,noexec,size=1g,mode=0700 \
    --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
    --env CLOUDSDK_CONFIG=/tmp/gcloud --env PYTHONDONTWRITEBYTECODE=1 \
    --mount "type=bind,src=$WORK/sa.json,dst=/gcs/sa.json,readonly" \
    --mount "type=bind,src=$WORK/bucket,dst=/gcs/bucket,readonly" \
    --mount "type=bind,src=$HERE/gcs-readback.py,dst=/check.py,readonly" \
    google/cloud-sdk:alpine timeout 840 python3 /check.py 2>&1 | tee "$WORK/check.log" || rc=$?
[ "$rc" -eq 0 ] || fail "remote readback failed (exit $rc); no remote bytes were written or deleted"
# A silent exit 0 proves nothing. Keep output streaming to the runner while
# also validating the one final receipt before this door prints its effect.
sed -n 's/^receipt: //p' "$WORK/check.log" > "$WORK/receipt.json"
jq_doc_file "$WORK/receipt.json" \
    && jq -e -s 'length == 1 and (.[0] | .result == "ok" and .stage == "verified"
      and .claim == "remote-archive-integrity" and .gzip_test == true and .completion_trailer == true
      and .verification_mode == "bounded-stream" and .gzip_test_exit_code == 0
      and (.generation | test("^[1-9][0-9]*$"))
      and (.object | test("^boss-[0-9]{8}-[0-9]{6}[.]sql[.]gz$"))
      and (.sha256 | test("^[a-f0-9]{64}$"))
      and .object_bytes > 0 and .downloaded_bytes == .object_bytes
      and .listed_pages >= 1 and .matching_dumps >= 1
      and (.checked_at | type == "string" and length > 0))' "$WORK/receipt.json" >/dev/null \
    || fail 'the check returned no single verified remote archive receipt'
echo 'check-gcs-backup: READ — remote archive integrity verified'
