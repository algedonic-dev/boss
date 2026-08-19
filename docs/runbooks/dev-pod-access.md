# Runbook: a terminal on the dev pod, from anywhere

**What this is:** the two buildable halves of "a terminal on a cluster
host works from anywhere" (packet `41f6f320`) — the pod exists and the
paths below are verified live (2026-08-19). The third half, terminal
access as a presence-gated *protocol*, is part of the compute-fabric
IT-protocols program and deliberately not here.

## The pod

`boss-dev` in namespace **`boss-dev`** (its own namespace, own
disposable StorageClass — see `infra/cluster/manifests/boss-dev.yaml`,
which the deploy runner converges with everything else). Two
containers: `dev` (the shell you want) and a Postgres sidecar on
`127.0.0.1:5432` — the blessed TestDb target; production is never
reachable from here.

Layout, measured:

| | |
|---|---|
| `/work/boss` | the clone, on a Longhorn PVC (survives pod restarts) |
| `/scratch` | node-local, ~188 GB free — `CARGO_TARGET_DIR` lives here |
| toolchain | cargo (via `~/.cargo/env` — see below), bun, tmux, psql |

**Cargo note:** `kubectl exec` shells don't source it. Start with
`. "$HOME/.cargo/env"` or prefix commands with it.

## Getting a terminal

Always land in tmux — a session survives its exec dying and a second
exec attaching (measured in
[dev-node-checkout](../design/dev-node-checkout.md)):

**From the home LAN** (a machine with the v2 kubeconfig):

```
kubectl --kubeconfig ~/talos-homelab/v2/kubeconfig \
  -n boss-dev exec -it deploy/boss-dev -c dev -- tmux new -As main
```

**From anywhere** (the path that needs only the boss-gcp key): jump to
the forge host and use its dockerized kubectl —

```
ssh -J boss-gcp david@10.20.0.15
sudo docker run --rm -it --network host \
  -v /home/david/kc.yaml:/kc:ro alpine/k8s:1.33.3 \
  kubectl --kubeconfig=/kc -n boss-dev exec -it deploy/boss-dev -c dev -- \
  tmux new -As main
```

(The kubeconfig lives at `/home/david/kc.yaml` — **not** `/tmp/kc.yaml`;
the access-recovery runbook's older pointer predates a `/tmp` clean.)

Detach with `C-b d`; the session keeps running. Reattach with the same
command — `new -As` attaches if `main` exists, creates it otherwise.

## What this is not

No browser path, no presence gate, no checkout ceremony — an operator
with the boss-gcp key gets a root-ish shell on the pod, full stop.
Those are exactly the protocol questions the compute-fabric program
owns (dev-node-checkout's approved design is the destination); this
page is the honest description of the door that exists today.
