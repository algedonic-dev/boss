#!/usr/bin/env bash
#
# push-step-plugins — put the Step UX bundles on the cluster NOW.
#
#   infra/push-step-plugins.sh            # push infra/step-plugins/*.js
#   KUBECONFIG=... infra/push-step-plugins.sh
#
# WHY THIS EXISTS. David, 2026-08-14: "Since Step UX is just a
# javascript bundle, do we have a way to deploy those faster?" We did
# not, and the full path was absurd for a CSS tweak: park a car, wait
# for the dock to reach the boarding threshold (or the 06:00/18:00
# window), 25-40 minutes of CI running the whole Rust suite for a file
# no Rust test reads, merge, converge, then a pod roll that takes the
# system of record dark for ~2 minutes. Hours, and an outage, to move
# one file.
#
# None of that is needed, and the reason is three facts that already
# hold:
#
#   1. The gateway reads the bundle with `tokio::fs::read` PER REQUEST
#      (boss-gateway/src/plugin_files.rs) — nothing is cached at boot,
#      so a changed file is served by the running pod immediately.
#   2. The bundles arrive as a ConfigMap mounted as a VOLUME with no
#      `subPath` (boss.yaml). Kubernetes propagates ConfigMap updates
#      into such a mount without restarting the pod; a subPath mount
#      would not, which is why that detail is load-bearing.
#   3. Cache-Control on the bundle is `private, max-age=60`.
#
# So updating the ConfigMap alone is a complete deploy of a plugin,
# visible within about a minute, with no image build and no pod roll.
#
# THIS IS A PREVIEW, NOT A BYPASS, and the difference matters. The tree
# stays canonical: `infra/forge/cluster-deploy-runner.sh` rebuilds this
# same ConfigMap from `infra/step-plugins/` on every converge, so
# whatever you push here is replaced by whatever main says the next
# time main moves. Push to see your change now; still ship the car.
# What you must NOT do is treat this as the way a bundle reaches
# production — that is the hand-applied drift the manifests README is
# about, and it survives exactly until the next merge.
#
# The generation command is deliberately identical in shape to the
# runner's, because two ways of building the same ConfigMap is the
# second definition that drifts (CLAUDE.md 9a). If you change one,
# change both.
set -euo pipefail

DIR="${BOSS_PLUGINS_SRC:-infra/step-plugins}"
NS="${BOSS_NAMESPACE:-boss}"

[ -d "$DIR" ] || { echo "push-step-plugins: no $DIR — run from the repo root" >&2; exit 1; }

args=()
names=()
for f in "$DIR"/*.js; do
    [ -e "$f" ] || continue
    args+=("--from-file=$(basename "$f")=$f")
    names+=("$(basename "$f")")
done
[ ${#args[@]} -eq 0 ] && { echo "push-step-plugins: no bundles in $DIR" >&2; exit 1; }

# Parse every bundle before shipping any of them. A syntax error here
# is a plugin that mounts as nothing on a surface an operator is
# already looking at, and the whole point of this path is that it
# skips CI.
if command -v node >/dev/null 2>&1; then
    for f in "$DIR"/*.js; do
        node --check "$f" >/dev/null || { echo "push-step-plugins: $f does not parse — refusing" >&2; exit 1; }
    done
    echo "push-step-plugins: ${#names[@]} bundles parse"
else
    echo "push-step-plugins: node not found — shipping WITHOUT a parse check" >&2
fi

kubectl create configmap step-plugins -n "$NS" "${args[@]}" \
    --dry-run=client -o yaml | kubectl apply -f -

echo "push-step-plugins: pushed ${names[*]}"
echo "push-step-plugins: mounted copies refresh within ~60s (kubelet sync);"
echo "                   browsers hold the old bundle up to 60s (Cache-Control)."
echo "push-step-plugins: the tree is still canonical — ship the car, or the"
echo "                   next converge replaces this with whatever main says."
