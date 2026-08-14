#!/usr/bin/env bash
#
# cluster-deploy-runner — converge the cluster onto forge main.
#
# The deployment half of the forge shipping protocol (directive
# 27ab7680): the conductor merges CI-green trains into forge main;
# this runner, on the forge host, notices main moved, builds the
# all-in-one image, pushes it to the forge registry, applies the
# tree's cluster manifests (infra/cluster/manifests/), and rolls the
# cluster deployment. Derived state reconverging on intent
# (deployment-as-network) — no ssh from the conductor, no shared
# credentials: each side touches only what it owns. Cluster config
# converges on forge main exactly like code does; hand-applied
# changes are drift (see infra/cluster/manifests/README.md).
#
# Install (forge host):
#   sudo cp infra/forge/cluster-deploy-runner.{service,timer} /etc/systemd/system/
#   sudo systemctl daemon-reload && sudo systemctl enable --now cluster-deploy-runner.timer
#
# Expects: a clone at $REPO with a `forgejo` remote; rootless docker
# (lingering enabled) logged into the registry; a kubeconfig at
# $KUBECONFIG_PATH; kubectl via the alpine/k8s container.
set -euo pipefail

# Run from a SNAPSHOT, never from the file this script is about to
# rewrite.
#
# This script lives inside $REPO, and a few lines below it runs
# `git checkout "$HEAD"` on that same repo. Bash does not read a script
# into memory; it reads incrementally and remembers a BYTE OFFSET. So
# when git replaces the file underneath a running invocation, bash
# carries on reading the new contents from the old offset — resuming
# mid-token, skipping a command, or executing a fragment of a line that
# happens to be syntactically valid. The failure is silent, unrepeatable
# and shaped by how much the diff moved the bytes, which makes it close
# to undiagnosable from the outcome alone.
#
# It has not bitten yet only because the file changed rarely and the
# offsets happened to survive. That is luck, not a property.
#
# `exec` into a copy makes the executing file structurally incapable of
# being rewritten: git can do whatever it likes to $REPO afterwards,
# because the bytes bash is reading are no longer reachable from there.
# The env var is the recursion guard and also carries the path so the
# snapshot can clean itself up on the way out — the trap belongs in the
# snapshot's own process, since `exec` replaces this one and would never
# run a trap set here.
if [ -z "${BOSS_RUNNER_SNAPSHOT:-}" ]; then
    snap="$(mktemp -t cluster-deploy-runner.XXXXXX)"
    cat "$0" > "$snap"
    BOSS_RUNNER_SNAPSHOT="$snap" exec bash "$snap" "$@"
fi
trap 'rm -f "$BOSS_RUNNER_SNAPSHOT"' EXIT

REPO="${BOSS_FORGE_REPO_DIR:-$HOME/boss}"
REGISTRY="${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"
KUBECONFIG_PATH="${BOSS_FORGE_KUBECONFIG:-$HOME/kc.yaml}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$HOME/.boss-last-built}"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"

cd "$REPO"
git fetch -q forgejo main
HEAD=$(git rev-parse --short forgejo/main)
LAST=$(cat "$STAMP_FILE" 2>/dev/null || echo none)

if [ "$HEAD" = "$LAST" ]; then
    echo "cluster-deploy-runner: forge main unchanged ($HEAD)"
    exit 0
fi

echo "cluster-deploy-runner: forge main moved $LAST -> $HEAD; building"
git checkout -q "$HEAD" 2>/dev/null || git checkout -qf "$HEAD"
docker build -q -f infra/oss-quickstart/Dockerfile -t "$REGISTRY:$HEAD" .
docker push "$REGISTRY:$HEAD"

K="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"

# Cluster config converges with the code: apply the tree's manifests
# idempotently (secrets are referenced by name and stay out-of-tree).
# Apply comes BEFORE the image roll so the tag built above — not the
# placeholder tag committed in boss.yaml — is what the cluster ends
# on. A failed apply aborts here (set -e): no stamp is written, the
# next timer run retries.
KM="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/cluster/manifests:/manifests:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
echo "cluster-deploy-runner: applying infra/cluster/manifests"
$KM apply -f /manifests

# StepPlugin bundles converge from the tree too (job d35aec77).
# Code converges in the image, config in the manifests above, schema
# in boss-init — but the `step-plugins` ConfigMap was built by a
# kubectl command someone ran by hand, so it converged with nothing.
# Adding a bundle to infra/step-plugins/ and landing a train delivered
# NOTHING: the row at /system/step-plugins pointed at a file that was
# never mounted, and the step rendered "No plugin registered" with no
# error anywhere. That is how seven seeded plugins came to be active
# with absent bundles, blocking eleven ready review-design steps.
#
# Regenerated from the directory every converge, so the mounted
# bundles are whatever the tree says. --dry-run=client | apply keeps
# it idempotent; README.md is excluded because it is documentation,
# not a bundle. Deliberately NOT committed as a manifest: it is a
# derived artifact whose sources are already in tree, and a committed
# copy would be the second definition that drifts (§9a).
KP="sudo docker run --rm --network host -v $KUBECONFIG_PATH:/kc:ro -v $REPO/infra/step-plugins:/plugins:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
# `-i`, and that single flag is the whole bug this replaces. The first
# version piped the generated ConfigMap into `$K apply -f -`, but $K is
# `docker run --rm` with no `-i`, so the container never attached
# stdin: apply read an empty document, said "error: no objects passed
# to apply", and the generator died with "write /dev/stdout: broken
# pipe". The runner has failed on every tick since — silently, because
# a failed systemd oneshot notifies nobody — leaving the cluster on the
# placeholder tag committed in boss.yaml while forge main moved on.
#
# The lesson is the one from the zsh/bash mixup earlier the same day:
# a command validated in a different environment than the one that
# runs it has not been validated. I checked the kubectl invocation
# against my own kubectl and never against the docker wrapper it
# actually runs through.
KAPPLY="sudo docker run --rm -i --network host -v $KUBECONFIG_PATH:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
echo "cluster-deploy-runner: converging the step-plugins ConfigMap"
PLUGIN_ARGS=""
for f in "$REPO"/infra/step-plugins/*.js; do
    [ -e "$f" ] || continue
    PLUGIN_ARGS="$PLUGIN_ARGS --from-file=$(basename "$f")=/plugins/$(basename "$f")"
done
if [ -n "$PLUGIN_ARGS" ]; then
    # shellcheck disable=SC2086
    $KP create configmap step-plugins -n boss $PLUGIN_ARGS \
        --dry-run=client -o yaml | $KAPPLY apply -f -
else
    echo "cluster-deploy-runner: no bundles in infra/step-plugins — leaving the ConfigMap alone"
fi

$K set image -n boss deploy/boss "boss=$REGISTRY:$HEAD"
$K patch deploy boss -n boss --type=json \
    -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$REGISTRY:$HEAD\"}]"
$K rollout status deploy/boss -n boss --timeout=420s

echo "$HEAD" > "$STAMP_FILE"
echo "cluster-deploy-runner: cluster on $REGISTRY:$HEAD"
