# BOSS OSS quickstart

Two install paths from a fresh clone to a BOSS SPA running
locally. Pick whichever matches your environment:

- **[Docker compose](#path-1-docker-compose)** — fastest if you
  already have Docker. No local Rust / Bun / Postgres / NATS
  install needed; the image carries everything. **Expect ~20–25
  min on a 2-vCPU VM** (the Rust image build dominates).
- **[Bare-metal local](#path-2-bare-metal-local-bootstrap)** —
  for operators who want to develop against the source tree.
  **Expect ~60–80 min on a 2-vCPU VM** (cold cargo build of 49
  Rust crates dominates); ~10–15 min on an 8-vCPU dev workstation
  with cached dependencies.

> ⚠  **Not production-ready.** Both paths run the whole platform
> on one machine in **demo mode**: anonymous visitors are minted
> a synthetic `audit-readonly` session at the gateway, so the
> SPA renders the live brewery immediately without forcing a
> login. The file-backed local-auth provider (real Argon2id-hashed
> credentials + signed session cookies + admin-issued password
> reset tokens) is still wired — log in at `/login` with the
> bootstrap-admin email + `change-me` to upgrade your session to
> `platform-admin` (full write access). No SSO, no MFA, no account
> lockout, no rate limiting, no edge-tier hardening. For production
> deployments see the **Post-release** section in
> [`TODO.md`](../../TODO.md) — Production Infrastructure Template,
> Integrated IAM (Authelia / OIDC), and Workflow modeling UX are
> queued there.

## Path 1: Docker compose

Needs Docker Engine + the Compose v2 plugin (`docker compose`, not
the legacy `docker-compose`). On a fresh Ubuntu/Debian VM:

```sh
curl -fsSL https://get.docker.com | sudo sh
```

Then:

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss/infra/oss-quickstart
cp .env.example .env
# edit .env — set BOSS_BOOTSTRAP_ADMIN_EMAIL=you@example.com
./preflight.sh          # readiness check: leftover volume, stale image, busy ports
docker compose up
```

`preflight.sh` is a non-destructive readiness check: it flags a leftover
`boss_postgres-data` volume, a stale cached `boss:latest` image (old code),
or a busy host port 4443/5432 from a previous run — each with the exact
`docker compose down -v` / `--build` command to clear it. This is the step
that catches "I ran it before and now `boss-init` errors with *relation
already exists*."

Three containers come up: `postgres:16`, `nats:2.10`, and a
`boss-services` container that runs every BOSS API binary plus the
gateway (which serves the SPA at `/`). A one-shot `boss-init`
container applies the schema and seeds your bootstrap-admin Employee
with role `platform-admin`; `boss-services` then publishes the
brewery tenant (JobKinds, accounts, vendors) and starts the sim,
which builds the demo live from an empty log.

When `boss-services` logs `all services up`, open
**http://localhost:4443**. The SPA lands you in `audit-readonly`
demo mode immediately — every projection renders, every write
returns 403. To get write access, click **Log in** (or hit
`/login` directly) and authenticate with the email you set in
`.env` + the default password `change-me`. That upgrades your
session to `platform-admin`. Rotate the password right after —
see [Authentication](#authentication) below.

The first `docker compose up` takes ~20-25 minutes on a 2-vCPU VM
(the Docker image build of the Rust workspace dominates). The SPA
starts sparse and fills in as the sim ticks. Subsequent runs start
in seconds because the image cache and Postgres volume persist.

Stop the stack with `docker compose down`. Add `-v` to wipe the
Postgres volume — the next `up` re-runs init and re-seeds.

## Path 2: Bare-metal local bootstrap

### Prerequisites

**System packages first** — on a fresh Ubuntu/Debian VM, the bun
installer needs `unzip` and the Rust build needs a C toolchain.
Install both before the per-tool table below:

```sh
sudo apt-get install -y curl ca-certificates unzip build-essential pkg-config libssl-dev git
```

(macOS users: `unzip` ships in the base system; install Xcode CLT
via `xcode-select --install` for the C toolchain.)

Then four tools running on `localhost`:

| Tool | Version | Install |
|---|---|---|
| Rust | stable | https://rustup.rs/ |
| Bun | 1.1+ | `curl -fsSL https://bun.sh/install \| bash` (requires `unzip`) |
| Postgres | 16+ on `:5432` | `apt install postgresql-16` / `brew install postgresql@16` |
| NATS | any | [download](https://nats.io/download/) a release binary, then run `nats-server -js` (JetStream is required) |

After installing Bun, **open a new shell** (or `source ~/.bashrc`)
so `bun` lands on your `PATH` — the installer modifies your shell
rc but it doesn't take effect in the current process.

The Postgres role `boss` (password `boss`) must exist as a
superuser — `bootstrap-boss.sh` creates the database but **not** the
role. Create it once before running the quickstart:

```sh
sudo -u postgres psql -c "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss';"
```

### Run it

```sh
git clone https://github.com/algedonic-dev/boss.git
cd boss
./infra/oss-quickstart/quickstart.sh
```

The script will:

1. Check the four prereqs above (bailing with install hints if any
   are missing), then run a non-destructive **preflight readiness
   check** that halts with a clear message if a prior run's BOSS
   services are still up or the gateway port 4443 is taken — pass
   `--skip-preflight` to override.
2. Prompt for your **bootstrap-admin email** — the seed Employee
   record that owns the platform-admin role. (Or pass
   `--email=you@example.com`.)
3. Build the workspace (~40 min cold on a 2-vCPU VM, ~10 min on
   an 8-vCPU dev workstation, ~30 s warm), then drop and recreate
   an empty local `boss` Postgres database. The first cold build
   dominates wall-clock; subsequent runs reuse `target/` and finish
   in seconds.
4. Build the SPA via Bun.
5. Insert your bootstrap-admin Employee into the live DB +
   audit_log so a future rebuild reproduces the row, and seed
   their `change-me` credential in
   `/var/lib/boss/auth/credentials.toml`.
6. Start every service as a background process (PIDs in
   `~/.boss-pids`) — including the brewery tenant seed (JobKinds,
   accounts, vendors) and the sim that builds the demo live, and the
   gateway on `127.0.0.1:4443`.

When it prints `Quickstart complete.`, open
**http://127.0.0.1:4443** — you should see Algedonic Ales' landing
page with the simulator clock ticking in the lower-right badge.
The gateway runs in demo mode (anonymous = `audit-readonly`), so
the SPA renders without a login. Click **Log in** to upgrade to
`platform-admin` using the bootstrap-admin email + `change-me`.

## What you're looking at

The brewery (Algedonic Ales) is the public OSS demo tenant. The
install seeds the reference data — employees, accounts, vendors,
recipes, equipment, the JobKind catalog — then starts the brewery
sim, which ticks ~1 sim-day per 86 wall-seconds and builds the rest
live: orders, work, invoices, ledger entries, projections. The SPA
is sparse on first load and fills in as the sim runs.

Try:

- `/ux/exec` — the executive dashboard.
- `/system/monitoring` — service health, deployment topology, ML
  oversight.
- `/system/kb` — architecture diagrams, ADRs, hardware/software
  reference.
- `/ux/jobs` — every Job in flight.
- `/system/workflows` — the JobKind catalog (read-only).
- `/system/job-kinds` — JobKind authoring (visible to your platform-admin
  role).

## Stop it

```sh
kill $(cat ~/.boss-pids)
```

## Re-run it

```sh
./infra/oss-quickstart/quickstart.sh --email=you@example.com
```

Re-runs auto-detect existing state: if the `boss` database already
has a populated `audit_log` (>1000 rows — i.e. the sim has been
running), the DB bootstrap is skipped and the script just rebuilds
the SPA + restarts services. To start the demo over from an empty
log, drop the DB first:

```sh
sudo -u postgres dropdb boss
./infra/oss-quickstart/quickstart.sh --email=you@example.com
```

The bootstrap-admin email upserts in either path.

## Exposing the stack to a public hostname

Both install paths land you on `127.0.0.1:4443`. The gateway
is HTTP-only — it does NOT terminate TLS, validate hostnames,
or rewrite the SPA's fetch origin. For a public deployment:

1. **Run a TLS-terminating reverse proxy in front** — Caddy,
   nginx, an ALB, a Cloudflare Tunnel — pointing at
   `127.0.0.1:4443`.
   The reference Caddyfile at `infra/caddy/Caddyfile` reads
   `BOSS_HOSTNAME` from env and proxies to the gateway:

   ```sh
   sudo BOSS_HOSTNAME=boss.example.com caddy run --config infra/caddy/Caddyfile
   ```

   Caddy fetches a Let's Encrypt cert via HTTP-01 challenge.
   The hostname must resolve directly to the VM (no proxy in
   between), or use `BOSS_HOSTNAME=localhost` for HTTP-only
   local testing.

2. **Set `BOSS_SESSION_KEY` to a strong random value** — see
   the next section. The default (`please-rotate-me-in-prod-do-
   not-leak`) is correctly named.

3. **Rotate the bootstrap-admin password.** See
   *Authentication* below.

The Docker compose stack does not bundle Caddy; bringing it up
is a separate concern outside the container. v1's framing is
"two installs that run on a single VM"; multi-tier production
deploys (HA gateway, separate TLS terminator, dedicated DB)
are tracked under the **Production Infrastructure Template**
TODO.

## Authentication

Both paths run the gateway with **demo mode + local-auth side
by side**. Demo mode (`BOSS_DEMO_MODE=1`) mints a synthetic
`audit-readonly` session for anonymous visitors — read every
projection, never write — so the SPA loads immediately on first
hit. Local-auth (`BOSS_AUTH_PROVIDER=local-auth`) gates the
`/login` route: the bootstrap-admin email + password upgrades
the cookie to a real `platform-admin` session with full write
access.

The bootstrap-admin credential is provisioned automatically on
both paths. Default password: `change-me`. The credential lives
in `/var/lib/boss/auth/credentials.toml` (Argon2id hashed).

Rotate it before exposing the stack to anything other than your
laptop:

```sh
# Docker:
docker compose exec boss-services boss-auth set you@example.com

# Bare-metal:
BOSS_AUTH_FILE=/var/lib/boss/auth/credentials.toml \
    target/release/boss-auth set you@example.com
```

`boss-auth` is the admin CLI for the file-backed credential
store:

```sh
boss-auth list                    # list every credentialed email
boss-auth add  alice@example.com  # onboard a new user (prompts for pw)
boss-auth set  alice@example.com  # rotate an existing user's pw
boss-auth remove alice@example.com
boss-auth verify alice@example.com  # exit 0 on match, 1 on miss
```

Set a **strong** `BOSS_SESSION_KEY` (Docker: in `.env`;
bare-metal: in your shell env before running `quickstart.sh`)
before deploying anywhere reachable — it's the HMAC key the
gateway uses to sign session cookies. The default value
(`please-rotate-me-in-prod-do-not-leak`) is correctly named.

To turn off demo mode and force a real login on every request,
unset `BOSS_DEMO_MODE` (Docker: remove the line from
`docker-compose.yml`; bare-metal: edit
`infra/bootstrap-local.sh`'s gateway env).

> ⚠  This is the v1 launch auth — file-backed credentials, no
> account lockout, no email-based password reset, no MFA.
> Production deployments will use Authelia (or any OIDC IDP)
> fronting the gateway via forward-auth headers; tracked under
> the **Integrated IAM** post-release entry in
> [`TODO.md`](../../TODO.md).

## Troubleshooting

**`pg_isready` fails.** Postgres isn't listening on
`127.0.0.1:5432`. Start it: `brew services start postgresql@16`
or `sudo systemctl start postgresql`.

**`could not connect to server: Connection refused` on NATS.**
Start `nats-server` on port 4222: `nats-server -js`.

**`error: linker 'cc' not found`.** Install `build-essential`
(Linux) or `xcode-select --install` (macOS).

**Bootstrap takes much longer than expected.** The first cargo
build does cold compile of ~150 crates (49 boss-* + their
transitive deps). On a 2-vCPU VM this is 40-50 minutes; on an
8-vCPU dev workstation closer to 10. Subsequent runs reuse
`target/` and finish in seconds. If you're evaluating on cloud
VMs, a 4+ vCPU instance halves the wait.

**`relation "..." already exists` / `boss-init exited with code 3`.**
A previous (often failed) install left an already-initialized Postgres
volume, or a cached `boss:latest` image is running older code against it.
Run `./infra/oss-quickstart/preflight.sh` to see what's lingering, then
clear it for a clean slate:

```sh
docker compose -f infra/oss-quickstart/docker-compose.yml down -v   # wipe the volume
docker compose -f infra/oss-quickstart/docker-compose.yml up --build # rebuild from current source
```

## Validating the brewery sim (maintainers)

The demo builds itself live, so there's nothing to fetch or load. To
check that a year of sim still reconstructs and reconciles cleanly (the
correctness gate maintainers run before a release):

```sh
sudo ./infra/postgres/validate-brewery-sim.sh
```

It drops the `boss` DB, prepares the brewery tenant
(`boss-brewery-sim prepare`), runs `boss-brewery-sim run` for 365
sim-days from 2025-04-01 with hard-fail (any non-2xx aborts), then
asserts every projection rebuilds from `audit_log`
alone (`failures=0`) and passes the conservation + dangling-FK integrity
checks. ~30 minutes on a 4-core box; watch the per-step echo to follow
along.

The source-of-truth inputs are the brewery seed files
(`examples/brewery/seeds/{job_kinds,tenant,accounts,vendors,parts,products,classes}.toml`)
plus the sim engine (`crates/tenants/boss-brewery-engine`), which ticks
one sim-year against the live API.

Short cycles for iteration:

```sh
# 14 sim-days — completes in ~5 min
sudo BOSS_REGEN_DAYS=14 ./infra/postgres/validate-brewery-sim.sh

# custom start date to exercise a specific cadence ramp
sudo BOSS_REGEN_DAYS=30 BOSS_REGEN_START=2025-07-01 \
    ./infra/postgres/validate-brewery-sim.sh
```

`--hard-fail` surfaces the failing request on stderr — common roots: a
JobKind step referencing a SKU/employee/account the tenant seed didn't
create, a side-effect handler error (empty line_items, inventory
underflow, FK violation), or service-bootstrap timing on a slow box.

To reset a running demo back to "seeded day 0" without a full regen, use
`infra/postgres/reset-to-baseline.sh` (host-level, drop + reseed) or the
in-app **Reset** button (trims the audit_log back to the seeded
baseline).

## What's next

Once you've kicked the tires:

- Read [`README.md`](../../README.md) for the platform thesis +
  architecture frame.
- Read [`docs/architecture-decisions.md`](../../docs/architecture-decisions.md)
  for every load-bearing design decision.
- Read [`examples/brewery/DOMAIN.md`](../../examples/brewery/DOMAIN.md)
  for how the brewery models its operations on BOSS primitives.
- Open issues / PRs at https://github.com/algedonic-dev/boss.
