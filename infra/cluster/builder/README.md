# Cluster builder image

The single container image build-1 needs (dev-cluster.md
§Bring-up mechanics): Rust toolchain + sccache client, run as the
workload of actions-runner-controller pods on the Talos cluster.

## Build (on the cluster, or any docker host)

    docker build -t boss-builder:latest infra/cluster/builder

## What still needs the hardware (Q1) before wiring

- **ARC install**: repo-scoped runner scale set; the registration
  token is the human credential the old join script refused loudly
  about — same rule here.
- **sccache server**: a Deployment + PVC; its service address goes
  into the runner pod spec as `SCCACHE_ENDPOINT`. Unset, sccache
  falls back to per-pod local disk — cold but functional.
- **Pod security** (Q3): the runner executes PR workflow code;
  namespace + securityContext decisions are recorded as
  pod-shaped in the design doc.
- **CI wiring**: once a runner is green, the rust job's `runs-on`
  gains the self-hosted labels — a one-line ci.yml change, made
  only after the runner has built main successfully once.

## Deliberately not here

- No bun / web toolchain: the web job is minutes on hosted runners;
  a second image variant earns its place when queue depth says so.
- No BOSS service images: build-1's job is CI. Services
  containerize only if Q4 moves the playground onto the cluster.

Untested until first contact with real hardware — expect and budget
the fix pass, per the design doc's standing honesty clause.
