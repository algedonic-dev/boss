# Runbook: BOSS operator procedures

Day-to-day and break-glass operations for a single BOSS VM.
Covers **backup, restore, deploy, rollback, and the handful of
incidents that actually page someone at 2am**. Keep it terse —
every procedure in this doc should fit on one screen so you can
scroll while someone else is on the phone.

Audience: whoever is `root@os-primary-1` (or equivalent). Assumes
the [CLI](../../crates/orchestrators/boss-cli/README.md) is installed, the user
can `sudo`, and the VM has the systemd units deployed by
`infra/deploy-services.sh`.

## Quick reference

```sh
boss status                      # are all services healthy?
boss backup                      # dump db + configs right now
boss deploy run <service>        # rebuild + restart one service
boss deploy run                  # rebuild + restart everything
boss deploy clean                # free disk before a big build
boss logs <service> --lines 200  # tail a service's journalctl
boss restart <service>           # no rebuild, just bounce
sudo ./infra/check-service-drift.sh   # services on disk vs in git
sudo ./infra/verify-smoke.sh          # DB sanity after a sim run
sudo ./infra/restore.sh <tarball>     # wipe + reload from backup
```

---

## Backup

### How backups work today

- Unit: `/etc/systemd/system/boss-backup.service` (oneshot) +
  `.timer` that fires daily at 03:00 UTC with a 5-minute random
  jitter.
- Script: `infra/backup.sh`.
- Output: `/var/backups/boss/boss-backup-YYYYMMDD-HHMMSS.tar.gz`.
- Retention: last 5 tarballs. Older ones are deleted by the same
  script on each run, so there's never a "backup disk full" alarm
  from this path.
- Content (what restore can reproduce):
  - `pg_dump boss` — full Postgres database.
  - `/etc/boss-gateway/` — gateway session key.
  - `/etc/boss-*.toml` — every per-service config.
  - `/etc/systemd/system/boss-*.service` — every unit file.

### Trigger a manual backup

```sh
boss backup          # or: sudo ./infra/backup.sh
boss backup --json   # scriptable output
```

Prints the tarball path. Takes 10–30 seconds on a healthy VM.

### Verify the last backup is sane

```sh
ls -lh /var/backups/boss/                         # newest should be <24h old
tar -tzf /var/backups/boss/boss-backup-*.tar.gz | head    # contents listing
```

If the newest backup is >24h old, the timer is broken — see
[Timers not firing](#timers-not-firing).

---

## Restore

### When to restore

- You ran an accidental `DROP TABLE` or `DELETE` and need the row
  state from earlier today.
- The VM disk died and you're on a fresh provision.
- A schema migration corrupted state and you want to rewind.

**Restore is destructive.** It drops the existing `boss` database
before reloading. Take a fresh backup of current state first so
you can compare if you need to.

### The procedure

```sh
boss backup                                       # snapshot current state first
sudo ./infra/restore.sh /var/backups/boss/boss-backup-YYYYMMDD-HHMMSS.tar.gz
sudo systemctl start boss-gateway boss-assets-api boss-commerce-api \
    boss-people-api boss-shipping-api boss-messages-api boss-inventory-api \
    boss-jobs-api boss-ledger-api boss-catalog-api
boss status
```

`restore.sh` stops the services it knows about, reloads Postgres,
copies configs back, then returns. It does **not** restart
services — the final `systemctl start` is deliberate so you can
inspect the restored state first.

### What restore does not cover

- **NATS jetstream state.** Messages in-flight at the time of
  backup are lost. Re-emission happens via event-sourced replay
  where the consumer supports it; otherwise the downstream
  projection is eventually-consistent.
- **TLS certs.** Caddy's Let's Encrypt material lives in
  `/var/lib/caddy/` and isn't in the tarball. On a new VM, Caddy
  re-issues on first start.
- **Secret rotation.** If the DB password,
  or session signing key has rotated *after* the backup, the
  restored configs will be stale — re-deploy with current secrets.

---

## Deploy a service

### The normal path

```sh
boss deploy run <service>
```

This rebuilds the binary (`cargo build --release -p <service>
--features postgres`), installs it to `/usr/local/bin/`, and
restarts the systemd unit. Output shows exactly which step ran.
It does not health-check the result — run `boss status` after to
confirm the service came back.

For a full deploy:

```sh
boss deploy run            # every service listed by `boss deploy list`
boss deploy web            # web frontend (Bun bundle)
```

### Deploy config + systemd (no rebuild)

If only the TOML config or unit file changed:

```sh
sudo ./infra/deploy-services.sh apply prod       # just prod
sudo ./infra/deploy-services.sh apply scratch    # just the +1000-port scratch stack
sudo ./infra/deploy-services.sh apply both       # both at once
sudo ./infra/deploy-services.sh check prod       # dry-run — diff only, no writes
```

The script is idempotent — running it when nothing has changed
prints "no changes" and exits 0. `check` mode is the gateway if
you're unsure what's about to happen.

### Post-deploy checks

```sh
boss status                                       # all 16+ services ok?
sudo ./infra/check-service-drift.sh              # disk vs git reconciled?
```

If `status` shows a red service, pull its logs:

```sh
boss logs <service> --lines 100
# or:
sudo journalctl -u boss-<service>-api --since "5 min ago"
```

---

## Deploy the web UI

Frontend changes land in the browser only after the Bun bundle is
rebuilt and installed to `/var/lib/boss-web/dist/` (the static dir
`boss-gateway` serves). The API-service deploy flow (`deploy-services.sh`
/ `boss deploy run`) does **not** touch the bundle.

```sh
sudo ./infra/deploy-web.sh
```

The script:

1. Runs `bun run build` in `apps/web/` as the invoking user (so
   `node_modules` + `dist/` ownership stays consistent).
2. Rsyncs the output to `/var/lib/boss-web/dist/` with `--delete` so
   old chunk files don't pile up.
3. No service restart needed — the gateway reads from the dir on
   every request.

### When to run it

Any time `apps/web/` changes hit `main`:

- New pages, nav edits, permission matrix changes.
- Type / hook changes that touch the TS build.
- Shared UI component edits.

If the UI looks stale after a frontend commit, check
`ls -la /var/lib/boss-web/dist/` — a timestamp older than your last
`apps/web/` commit is the smoking gun.

### Hard refresh

A browser that has the old `index.html` cached will keep loading
the old chunk filenames even after the dist is updated. If users
report the UI hasn't changed, have them hard-refresh
(Ctrl+Shift+R / Cmd+Shift+R) to pick up the new `index.html`.

---

## Rollback a deploy

**Git is the rollback tool, not the binary.** Every commit on
`main` is deployable; rolling back means `git checkout <sha>` + a
normal deploy. Don't rely on binary caches — they get cleaned by
`boss deploy clean` or routine VM upgrades.

### The procedure

```sh
# 1. Find the last-known-good commit.
git log --oneline -20

# 2. Check out that commit (detached HEAD is fine for deploy).
git checkout <sha>

# 3. Rebuild + restart.
boss deploy run <service>    # or `boss deploy run` for everything

# 4. Verify.
boss status
boss logs <service> --lines 50

# 5. Once you're sure the rollback is stable, return the tree to
#    main so subsequent work doesn't branch from a detached state.
git checkout main
```

### When a rollback alone is not enough

A deploy can leave the database in a state the older binary can't
read — new migrations, new enum values, new required columns. If
the rollback binary crashes at startup with a schema mismatch:

1. Either **forward-fix**: keep the newer binary, patch the bug,
   re-deploy.
2. Or **restore**: accept the data loss and restore from the last
   backup that predates the breaking migration (see
   [Restore](#restore) above).

There's no "downgrade migration" path in BOSS today — migrations
are forward-only. If this comes up repeatedly, add per-migration
rollback SQL; don't invent on the fly during an incident.

---

## Incidents

### Service won't start

```sh
sudo systemctl status boss-<service>-api          # what does systemd say?
boss logs <service> --lines 200                   # last 200 journal lines
```

Common causes:

- **Config drift.** `/etc/boss-<service>-api.toml` references an
  env var or secret that isn't set. Fix: re-run
  `sudo ./infra/deploy-services.sh apply prod` to regenerate.
- **DB unreachable.** Postgres is down or the credentials are
  wrong. `sudo systemctl status postgresql`. If the DB is up,
  test the connection string from the config file directly.
- **Port conflict.** Scratch stack and prod both try to bind.
  Scratch ports are prod + 1000. If either env is running the
  wrong service on the other's port, `ss -tlnp | grep boss-`
  shows which PID holds what.
- **Binary crash on start.** Usually a panic from a breaking
  schema change or missing feature flag. `boss logs` shows the
  panic; the fix is almost always re-deploy with
  `--features postgres` or apply the pending migration.

### Postgres out of connections

```sh
sudo -u postgres psql -c "SELECT count(*) FROM pg_stat_activity;"
sudo -u postgres psql -c "SELECT pid, state, query_start, query \
    FROM pg_stat_activity WHERE state != 'idle' ORDER BY query_start;"
```

If a runaway query is holding connections, `pg_terminate_backend(<pid>)`
kills it. The gateway + each service opens a small bounded pool
(≤20 conns), so a connection storm usually means a single caller
in a tight loop — find that caller before just killing connections.

### Gateway won't accept login

```sh
sudo journalctl -u boss-gateway --since "5 min ago"
```

Typical failures:

- `BOSS_AUTH_FILE missing` — the credentials file path is wrong or
  the file doesn't exist. Default is
  `/var/lib/boss/auth/credentials.toml`. Bootstrap it with
  `boss-auth add <email>` (the credentials-file admin CLI shipped
  inside `boss-gateway`).
- `invalid credentials` returned to the SPA — confirm the email +
  password against the file with `boss-auth verify <email>`.
- Cert expired — Caddy didn't renew. `sudo systemctl status caddy`
  and check `/var/log/caddy/access.log`.

If you want short-lived SSH certs + a central revocation list for a
fleet of hosts (the pre-v0.1 SSH-CA flow), restore the blueprint at
`infra/blueprints/ssh-ca/`.

### Period-lock wall

Manual journal entry or sim run returns `period locked`. By
design — a month is closed. Options:

```sh
# Unlock a month (operator-tier action).
curl -X POST http://127.0.0.1:7080/api/ledger/periods/<id>/unlock

# Or, the in-app surface: /finance → Periods → Unlock.
```

Relock after the backdated entry posts so the period checksum
stays stable.

### Scratch stack stuck

Scratch is disposable. Fastest recovery:

```sh
sudo systemctl stop boss-*-api-scratch
sudo -u postgres psql -c "DROP DATABASE IF EXISTS boss_scratch;"
sudo -u postgres psql -c "CREATE DATABASE boss_scratch OWNER boss;"
/opt/boss/infra/postgres/apply-schema.sh | sudo -u postgres psql -d boss_scratch
sudo systemctl start boss-*-api-scratch
```

The scratch DB carries no operator-relevant state — `drop + create`
is always safe. **Do not run this on `boss`** (the production DB).
The user has in-flight UI state there.

### Timers not firing

Backup timer, ML-generate timer, or any future scheduled task not
running on schedule:

```sh
systemctl list-timers --all | grep boss           # all tracked?
sudo systemctl status boss-backup.timer           # is it active?
journalctl -u boss-backup --since "2 days ago"    # why did the last run fail?
```

If the timer is `inactive (dead)`, it was manually stopped — re-enable:

```sh
sudo systemctl enable --now boss-backup.timer
```

If the timer fired but the service exited non-zero, the failure
is in `backup.sh`, not systemd — run the script by hand and read
the error.

### Disk filling up

```sh
df -h /
du -sh /var/lib/postgresql /var/backups/boss /opt/boss/target
boss deploy clean                                 # drop cargo debug artifacts
```

Typical culprits, ordered by what each usually contributes:

- **`target/`** — `cargo build` artifacts. `boss deploy clean`
  wipes debug builds but keeps release binaries. Safe to run any
  time.
- **`pg_wal/`** — Postgres write-ahead log. If this is large,
  replication is lagging or the archive command is failing;
  otherwise Postgres rotates it.
- **`/var/backups/boss/`** — capped at 5 tarballs; shouldn't
  grow unboundedly. If it does, the retention trim in
  `backup.sh` failed.
- **`journalctl`** — `sudo journalctl --vacuum-size=1G` trims
  old journal data.

---

## Post-incident checklist

After any break-glass intervention:

- [ ] `boss status` shows every service green.
- [ ] `./infra/check-service-drift.sh` passes.
- [ ] `boss backup` succeeds — a fresh backup is on disk.
- [ ] If DB was touched: pick one revenue figure on `/finance` and
      spot-check it against the TB (`GET /api/ledger/trial-balance`).
- [ ] Write down what happened in `docs/runbooks/incidents/` (one
      short file per incident, dated). The goal isn't a polished
      postmortem — it's so the next operator seeing the same
      pattern has prior art.

## Related runbooks

- [`infra/verify-smoke.sh`](../../infra/verify-smoke.sh) — sim
  output sanity check.
- [`infra/verify-replay.sh`](../../infra/verify-replay.sh) — full
  demo replay calibration.
