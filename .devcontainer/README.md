# Dev container / Codespaces

A ready-to-build BOSS workspace — Rust (edition 2024) + Bun + Postgres 16
+ NATS (JetStream) — so you can clone and start hacking without setting up
a toolchain.

## Open it

- **GitHub Codespaces:** repo → **Code ▾** → **Codespaces** → **Create
  codespace**. (Pick a 4-core machine or larger — the workspace build is
  big; see the note below.)
- **VS Code locally:** install the *Dev Containers* extension, open the
  repo, then **Dev Containers: Reopen in Container**. Needs Docker.

First launch builds the image and runs [`post-create.sh`](post-create.sh)
(toolchain components, `cargo fetch`, schema apply, `bun install`) — a few
minutes. It prints a cheat-sheet when it finishes.

## What's inside

| Piece | Provided by |
|-------|-------------|
| Rust stable (edition 2024), rustfmt, clippy, rust-analyzer | `Dockerfile` (base image) |
| Build deps (cmake, clang, pkg-config, libssl, postgres-client, jq) | `Dockerfile` |
| Bun (web toolchain) | `Dockerfile` |
| Postgres 16 + NATS (JetStream) | `docker-compose.yml` services |
| Connection env (`DATABASE_URL`, `NATS_URL`, `BOSS_TEST_POSTGRES_ADMIN_URL`, `PG*`) | `docker-compose.yml` |

The app container reaches Postgres at host **`postgres`** and NATS at
**`nats`** — the same `DATABASE_URL` / `BOSS_NATS_URL` wiring the
[OSS quickstart](../infra/oss-quickstart/) uses. Bare `psql` connects to
the `boss` DB (the `PG*` env is set), and `cargo test --all-features`
finds Postgres via `BOSS_TEST_POSTGRES_ADMIN_URL`.

## Common tasks

```bash
# Rust — the same gates CI runs
cargo build --workspace
cargo clippy --workspace --all-features --tests -- -D warnings
cargo test --all-features
cargo fmt -- --check

# Web (apps/web)
bun run typecheck
bun run build
bun run dev          # hot-reloading SPA dev server (port 3000)

# Reset the DB to an empty schema (boss is the postgres superuser here —
# no sudo; the host scripts' sudo -u postgres path is for a local Postgres)
dropdb boss && createdb boss && infra/postgres/apply-schema.sh | psql
```

To run the full prebuilt brewery demo (not needed for development), use
the [OSS quickstart](../infra/oss-quickstart/).

## Notes

- **Machine size.** The ~60-crate workspace's debug/test build is large;
  `devcontainer.json` requests 4 CPUs / 16 GB / 64 GB and sets
  `CARGO_PROFILE_DEV_DEBUG=0` (matching CI) to keep the build off the
  disk ceiling. The default 2-core / 32 GB Codespace runs out of space
  mid-`cargo test`.
- **First compile** is several minutes (cold target dir); subsequent
  builds are incremental and fast.
