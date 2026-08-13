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

$K set image -n boss deploy/boss "boss=$REGISTRY:$HEAD"
$K patch deploy boss -n boss --type=json \
    -p "[{\"op\":\"replace\",\"path\":\"/spec/template/spec/initContainers/0/image\",\"value\":\"$REGISTRY:$HEAD\"}]"
$K rollout status deploy/boss -n boss --timeout=420s

echo "$HEAD" > "$STAMP_FILE"
echo "cluster-deploy-runner: cluster on $REGISTRY:$HEAD"
