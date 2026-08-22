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
| toolchain | cargo (`/usr/local/cargo/bin` — see below), bun, tmux, psql |

**Cargo note:** this image installs the toolchain system-wide, not
per-user — there is no `~/.cargo/env` to source (an earlier revision
of this page said otherwise; a session lost time to it 2026-08-21).
Export instead:

```
export PATH=/usr/local/cargo/bin:$PATH RUSTUP_HOME=/usr/local/rustup
```

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

## Running gates here (this is the gate host now)

Full gates run in this pod, in tmux, since 2026-08-21. The working
recipe, every line earned:

```
cd /work/boss
export PATH=/usr/local/cargo/bin:$PATH RUSTUP_HOME=/usr/local/rustup \
  PLAYWRIGHT_BROWSERS_PATH=/work/.ms-playwright \
  CARGO_TARGET_DIR=/scratch/target RUST_TEST_THREADS=4
BOSS_GATE_RECEIPT=/scratch/receipt-<branch>.json ./infra/gate.sh
```

- **`PLAYWRIGHT_BROWSERS_PATH` on the PVC.** A pod restart wipes the
  container layer: browsers installed to the default `~/.cache` path
  vanish, and the mocked suite dies mid-gate. `/work` survives.
  Restarts also wipe the browsers' *system* deps — reinstall needs
  `/etc/apt/apt.conf.d/99-pod-no-sandbox` (`APT::Sandbox::User
  "root";` + `Dir::Cache::archives "/tmp/apt-cache";`,
  `DEBIAN_FRONTEND=noninteractive`) because apt's sandbox user cannot
  read the layer. The durable fix is baking the deps into the CI
  image; until then this is the recovery path (2026-08-21).
- **`RUST_TEST_THREADS=4`.** The pg sidecar's `/dev/shm` is the
  container-default 64Mi; unthrottled parallel pg suites exhaust it
  ("could not resize shared memory segment … No space left on
  device"), a backend dies, and postgres drops into crash recovery
  MID-GATE — which then fails *unrelated* suites ("database system is
  in recovery mode"). That signature means the environment, not the
  branch (it read as three different cars failing on 2026-08-22
  before the common cause surfaced). `fix/dev-pg-gets-real-shm`
  mounts a 1Gi memory-backed `/dev/shm`; the throttle stays a good
  idea until that lands and the pod restarts.
- ~~Receipts in `/scratch/receipt-*.json` survive the gate, not the
  pod.~~ **Superseded 2026-08-22 evening:** receipts and logs go to
  `/work/gate-out/` — the PVC. `/scratch` is an emptyDir and the pod
  was replaced three times in one day (a converge at 03:45Z, then
  cp-2 itself bouncing twice under gate load); every `/scratch`
  receipt died with it.

## The gate-runner protocol (2026-08-22, after losing a queue twice)

A gate queue that lives only in tmux + emptyDir is invisible state —
when the pod died, six queued gates vanished and a human asking "how
are we looking" was the detection mechanism. The rules that fix that:

1. **Every gate run is a `gate-run` packet**, registered BEFORE
   launch (branch, sha, receipt slug, `due_on` a few hours out) and
   closed with a verdict — `green`, `failed`, or `lost`; `lost` is an
   honest verdict. An overdue open `gate-run` packet IS the alarm.
2. **Receipts and logs on the PVC** (`/work/gate-out/`), never
   `/scratch`.
3. **Throttle the chain**: `CARGO_BUILD_JOBS=6 RUST_TEST_THREADS=4`
   and `nice -n 10`. Unthrottled, a six-gate chain starved etcd into
   failing readiness (18:10Z) and then bounced cp-2's kubelet
   outright (19:17:49Z) — the control plane shares this node's disk
   and memory. The manifest-side backstop (container resource
   limits) is `fix/dev-pod-cannot-eat-the-node`.
4. **After any pod replacement**: playwright's system deps are gone
   (container layer). Reinstall via direct `apt-get -o
   APT::Sandbox::User=root -o Dir::Cache::archives=/tmp/apt-cache
   install …` and verify by RUNNING the browser binary — the `bunx
   playwright install-deps` path exits nonzero here, and piping its
   output can mask that.

## What this is not

No browser path, no presence gate, no checkout ceremony — an operator
with the boss-gcp key gets a root-ish shell on the pod, full stop.
Those are exactly the protocol questions the compute-fabric program
owns (dev-node-checkout's approved design is the destination); this
page is the honest description of the door that exists today.
