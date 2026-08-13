# Cluster manifests — BOSS on the dev cluster (namespace `boss`)

The Kubernetes manifests that configure the cluster BOSS — the
system of record. This directory is the single source of truth for
cluster config, and it converges the same way code does:

**The rule**

- Cluster config changes land as cars through the train, like any
  other change. Edit a manifest here, ship it, done.
- On every converge, the runner on the forge host
  (`infra/forge/cluster-deploy-runner.sh`) runs
  `kubectl apply -f` on this directory from the freshly fetched
  tree — idempotently, before rolling the image, so the tag it just
  built supersedes the placeholder tag committed in `boss.yaml`.
- **Hand-applied changes are drift.** They survive only until the
  next converge, and the next converge is at most ten minutes after
  the next merge to forge main. If it matters, it goes through the
  train.
- **Secrets never live here.** Every manifest references its
  secrets by name only (`boss-secrets`, `boss-oidc`, `resend`,
  `boss-backup-key`, `forgejo-registry`, `cloudflare-api-token`,
  `boss-tls`); the Secret objects themselves are created out-of-band
  and stay out-of-tree.

**Code, config and schema all converge from the tree, every deploy**

Config converges here, code converges in the image — and the database
schema converges the same way. The `boss-init` initContainer runs
`infra/postgres/migrate.sh` in manifest order on *every* pod start,
against a fresh database and an existing one alike; the runner applies
only manifest entries missing from `schema_migrations`, prints each file
it applied plus an `applied N, already recorded M, of K manifest
entries` summary, and fails the container — and so the rollout — if any
migration errors. It used to skip a database that already had a schema,
which is how four migrations (112, 113, 114, 116) accumulated unapplied
on 2026-08-13 while the image and the manifests rolled forward: the
station registry shipped, the deploy reported success, and
`GET /api/stations` answered 500 `relation "stations" does not exist`.
**Hand-applied schema is drift**, exactly like a hand-applied manifest —
it stabilises the box for an hour and hides the fact that the tree and
the cluster disagree. A schema change is a new file appended to
`infra/postgres/schema/manifest.txt`, shipped through the train, never
an edit to a file that has already been applied (the runner refuses
those by name).

**What's here**

| file | what it is |
|---|---|
| `boss.yaml` | The single-pod BOSS topology: namespace, Postgres, NATS, the boss deployment, gateway Service on LB 10.20.0.30 |
| `boss-jobs-internal.yaml` | Machine door for the SoR jobs API — LB 10.20.0.34:7900 for the conductor and operator tooling |
| `boss-tls.yaml` | One-shot Jobs: DNS A record + lego Let's Encrypt cert for boss.algedonic.dev (publishes secret `boss-tls`) |
| `boss-tls-front.yaml` | Caddy TLS front terminating boss.algedonic.dev on LB 10.20.0.33 |
| `boss-backup.yaml` | Nightly pg_dump CronJob, 14-day PVC retention, offsite ship to boss-gcp |

Note on the one-shot Jobs in `boss-tls.yaml`: `kubectl apply` on an
existing completed Job with an unchanged spec is a no-op; the Jobs
re-run only if deleted from the cluster (deliberate — deleting the
cert Job is how you force a re-issue, mindful of Let's Encrypt
duplicate-cert limits).

**What's deliberately not here**

- Talos machine configs, kubeconfig, talosconfig — they embed
  cluster PKI and credentials; they stay in the operator's
  out-of-tree home (`~/talos-homelab/v2/`).
- Non-BOSS cluster infrastructure (Kanidm, cert-manager installs,
  Longhorn, Cilium/MetalLB pools, the lego/Origin CA issuers) —
  owned by the cluster, not by BOSS.
- `step-plugins.yaml` — a generated ConfigMap (72KB of JS built
  from `infra/step-plugins/*.js` via the `kubectl create configmap`
  command documented in its header). The sources are already in
  tree; committing the derived artifact would be a second copy that
  drifts (CLAUDE.md §9a).
