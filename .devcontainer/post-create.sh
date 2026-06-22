#!/usr/bin/env bash
# Dev-container first-run setup. Idempotent — safe to re-run.
#   - ensure rustfmt + clippy components
#   - warm the cargo registry (first compile is still a few minutes)
#   - apply the Postgres schema to the boss DB
#   - install the web app's deps
set -uo pipefail
cd /workspaces/boss

echo "==> rustfmt + clippy components"
rustup component add rustfmt clippy >/dev/null 2>&1 || true

echo "==> cargo fetch (warming the registry)"
cargo fetch || echo "   (cargo fetch failed — retry after the container settles)"

echo "==> applying Postgres schema (psql → the 'postgres' service via PG* env)"
# The postgres service auto-creates the boss DB (POSTGRES_DB=boss); the
# schema is idempotent (CREATE ... IF NOT EXISTS), so re-runs are safe.
if infra/postgres/apply-schema.sh | psql -q 2>/dev/null; then
  echo "   schema applied to boss"
else
  echo "   (schema apply skipped — is the postgres service up? re-run: infra/postgres/apply-schema.sh | psql)"
fi

echo "==> web deps (bun install)"
( cd apps/web && bun install ) || echo "   (bun install failed — re-run in apps/web)"

cat <<'EOF'

────────────────────────────────────────────────────────────────────
 BOSS dev container ready.  (Postgres → host "postgres", NATS → "nats";
 $DATABASE_URL / $NATS_URL are set, and bare `psql` just works.)

 Build + gate the workspace:
   cargo build --workspace
   cargo clippy --workspace --all-features --tests -- -D warnings
   cargo test --all-features
   cargo fmt -- --check

 Web app (apps/web):
   bun run typecheck
   bun run build
   bun run dev            # hot-reloading SPA dev server (port 3000)

 Reset the DB to an empty schema (boss is the postgres superuser here,
 so no sudo — the host scripts' `sudo -u postgres` path is for a *local*
 Postgres and isn't used in this container):
   dropdb boss && createdb boss && infra/postgres/apply-schema.sh | psql

 Run the full prebuilt brewery demo: see infra/oss-quickstart/.
────────────────────────────────────────────────────────────────────
EOF
