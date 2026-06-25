#!/usr/bin/env bash
# Unified deploy script for the Boss API services.
#
# Replaces deploy-scratch-services.sh and the scattered per-service
# setup.sh files. One data table, two environments:
#
#   sudo ./infra/deploy-services.sh prod
#   sudo ./infra/deploy-services.sh scratch
#   sudo ./infra/deploy-services.sh both
#
# Services:
#   - Paired (prod + scratch): shipping, messages, inventory,
#     commerce, people, assets, catalog, calendar, jobs. These share
#     a DB + NATS and the scratch variant runs on +1000 ports
#     against boss_scratch.
#   - Solo (prod only): sim, ml, docs. No scratch variants (sim is
#     the scratch driver; ml/docs are read-only stateless consumers).
#
# Per run the script:
#   1. Writes /etc/boss-<name>-api[-scratch].toml
#   2. Writes /etc/systemd/system/boss-<name>-api[-scratch].service
#   3. systemctl daemon-reload
#   4. systemctl enable --now each unit
#   5. Probes /api/<name>/health and reports the status code
#
# Idempotent — safe to re-run. Use `check` mode to diff generated
# files against what's on disk without writing anything:
#
#   sudo ./infra/deploy-services.sh check prod
#   sudo ./infra/deploy-services.sh check scratch
#   sudo ./infra/deploy-services.sh check both

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <prod|scratch|both|check> [prod|scratch|both]" >&2
    exit 2
fi

MODE="apply"
if [[ "${1:-}" == "check" ]]; then
    MODE="check"
    shift
    if [[ $# -lt 1 ]]; then
        echo "usage: $0 check <prod|scratch|both>" >&2
        exit 2
    fi
fi

TARGET="${1:-}"
case "$TARGET" in
    prod|scratch|both) ;;
    *)
        echo "error: target must be 'prod', 'scratch', or 'both', got '$TARGET'" >&2
        exit 2
        ;;
esac

NATS_URL="nats://127.0.0.1:4222"
# Services connect directly to PG :5432. The roster has grown to ~24
# services × sqlx pool-10, so the cluster runs with max_connections=400
# (raised from PG's default 100 via `ALTER SYSTEM SET max_connections`).
# At the default 100 a high-warp regen compresses a sim-year of writes
# into minutes, every pool tries to fill at once, policy-api gets starved
# of DB connections, and jobs-api fail-closes its policy checks (403
# policy-unreachable) — which the JetStream redelivery layer then
# amplifies into sustained saturation. 400 gives the pools room so the
# contention stays in the transient-blip regime redelivery is built for.
# A pgbouncer-in-front-of-PG layer was prototyped + removed pre-v0.1 (the
# isolation pays off at much larger concurrency than this has; the SCRAM
# image + statement-cache compatibility surface isn't worth carrying).
PROD_DB_URL="postgres://boss:boss@127.0.0.1/boss"
SCRATCH_DB_URL="postgres://boss:boss@127.0.0.1/boss_scratch"
REPO_ROOT="/opt/boss"

# Port tables sourced from `boss-ports-list` — single source of
# truth shared with the Rust binaries (boss_ports::PAIRED / SOLO).
# Falls back to a hardcoded snapshot if the binary isn't built yet
# (fresh checkout, pre-build); the snapshot has to stay in sync if
# you edit it directly. The 7060/7250 collision (commits `bb60c58`
# + `8bf0f0a`) was caused by exactly this kind of hand-sync drift.
PORTS_BIN="$REPO_ROOT/target/release/boss-ports-list"
if [[ -x "$PORTS_BIN" ]]; then
    mapfile -t PAIRED_SERVICES < <("$PORTS_BIN" --paired)
    mapfile -t SOLO_SERVICES   < <("$PORTS_BIN" --solo)
else
    echo "warning: $PORTS_BIN not built; using fallback snapshot" >&2
    PAIRED_SERVICES=(
        "shipping:7100:8100"
        "messages:7200:8200"
        "inventory:7300:8300"
        "commerce:7400:8400"
        "people:7500:8500"
        "assets:7600:8600"
        "catalog:7750:8750"
        "calendar:7860:8860"
        "jobs:7900:8900"
    )
    SOLO_SERVICES=(
        # sim:7060 retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
        # clock:7060 reclaimed 2026-05-30 — the authoritative "what
        # time is it" service. See boss-clock + boss-clock-client.
        "clock:7060"
        "ml:7070"
        "ledger:7080"
        "content:7090"
        # events:7150 — audit_log tail/stream/export. Split out of
        # boss-people-api 2026-06 (see crates/core/boss-events/src/
        # bin/boss_events_api.rs + boss-ports.events entry).
        "events:7150"
        "policy:7250"
        "docs:7050"
        # accounts:7550 — accounts + account_notes + account_team +
        # account_next_actions + account_risk_scores + support_cases.
        # Split out of boss-people-api 2026-06; mirrors the
        # one-binary-per-domain pattern every other core domain uses.
        "accounts:7550"
        # dispatcher:7950 — auto-assigns ready Steps to role-matched
        # Employees by subscribing to jobs.step.* NATS events. Core
        # service so sim runs + real-human runs go through the same
        # assignment path (no sim-side dispatch).
        "dispatcher:7950"
        "classes:7800"
        "locations:7820"
        "subject-kinds:7830"
        "products:7840"
    )
fi

# Timer + service unit pairs. Each entry is
# `unit-stem:source-dir-relative-to-infra` — the source dir
# holds `<unit-stem>.service` + `<unit-stem>.timer`. The deploy
# loop installs both, daemon-reloads, and `enable --now`s the
# timer (the service is `Type=oneshot`, fires from the timer).
#
# Adding a new timer = author the .service + .timer in the
# right place under infra/, then add a row here. New timers
# land via `sudo ./infra/deploy-services.sh prod` instead of a
# `sudo install` treadmill that's been the source of every
# "this timer was authored but never installed" gap so far
# (audit-integrity, ml-inference-batch, ledger-recognize,
# conservation-invariants — all caught by hand).
TIMERS=(
    "boss-messages-events-purge:."
    "boss-audit-integrity-check:."
    "boss-ledger-recognize:."
    "boss-ml-inference-batch:ml"
    "boss-conservation-invariants:lint"
    "boss-files-gc:."
    # boss-backup deferred — backup script destination + retention
    # policy needs review before enabling on a fresh deploy.
)

# Long-running daemons that aren't `boss-*-api` services. Each
# entry is `unit-stem:source-dir-relative-to-infra`. The deploy
# loop installs the unit, installs the binary, and `enable --now`s
# the service. Distinct from TIMERS (oneshot + .timer) and the
# port-table-driven *-api services.
#
# v1.0.10 F15: boss-step-effects-runner retired — step-completion
# side effects now route through the dispatcher's rule registry
# (infra/dispatcher/rules.toml).
DAEMONS=()

port_of() {
    local name="$1" env="$2"
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r n prod scratch <<<"$entry"
        if [[ "$n" == "$name" ]]; then
            [[ "$env" == "prod" ]] && echo "$prod" || echo "$scratch"
            return 0
        fi
    done
    for entry in "${SOLO_SERVICES[@]}"; do
        IFS=: read -r n prod <<<"$entry"
        if [[ "$n" == "$name" ]]; then
            echo "$prod"
            return 0
        fi
    done
    echo "port_of: unknown service '$name'" >&2
    return 1
}

# Human-readable description for a service unit.
description_of() {
    case "$1" in
        shipping)  echo "Shipping API" ;;
        messages)  echo "Messages API" ;;
        inventory) echo "Inventory API" ;;
        commerce)  echo "Commerce API" ;;
        people)    echo "People API" ;;
        assets)     echo "Assets API" ;;
        catalog)   echo "Catalog API" ;;
        calendar)  echo "Calendar API" ;;
        jobs)      echo "Jobs API (coordination primitive)" ;;
        sim)       echo "Simulator API" ;;
        ml)        echo "ML Platform API" ;;
        ledger)    echo "Ledger API" ;;
        content)   echo "HR Content API" ;;
        policy)    echo "Policy API" ;;
        clock)     echo "Clock API" ;;
        docs)          echo "Docs API" ;;
        classes)       echo "Class Registry API" ;;
        locations)     echo "Locations Registry API" ;;
        subject-kinds) echo "Subject Kind Registry API" ;;
        products)      echo "Finished Product Catalog API" ;;
        events)        echo "Audit Log Read API (tail / stream / export)" ;;
        accounts)      echo "Accounts API (notes / team / next-actions / risk / cases)" ;;
        dispatcher)    echo "Dispatch Service (auto-assigns ready Steps to role-matched Employees)" ;;
        simulator)     echo "Simulator UX service (SPA + control API)" ;;
        *)             echo "$1 API" ;;
    esac
}

# Emit the TOML config body for a paired service in a given env.
emit_paired_config() {
    local name="$1" env="$2"
    local port db_url
    port=$(port_of "$name" "$env")
    [[ "$env" == "prod" ]] && db_url="$PROD_DB_URL" || db_url="$SCRATCH_DB_URL"

    cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$db_url"
http_bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"
EOF
    case "$name" in
        commerce)
            echo "people_api_url = \"http://127.0.0.1:$(port_of people "$env")\""
            # Hard-requires the Class registry: the invoice status
            # CHECK was retired for registry-backed validation
            # (subject_kind='invoice', member_attribute='status').
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        messages)
            # Daily messages_events retention sweep. Mirrors the
            # `[messages] events_retention_days` row in
            # examples/<tenant>/seeds/tenant.toml; the deploy
            # surface lifts it into /etc/boss-messages-api.toml so
            # the boss-messages-events-purge binary (and its
            # systemd timer) can read it. Leave commented to
            # disable the sweep.
            echo "events_retention_days = 90"
            # Hard-requires the Class registry: the message kind
            # CHECK was retired for registry-backed validation
            # (subject_kind='message', member_attribute='kind').
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        people)
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            # boss-people-api hard-requires classes + locations
            # registry URLs at startup (the schema CHECKs were
            # retired in favor of registry-backed validation).
            # Both are prod-only solo services, so the same port
            # serves prod + scratch.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            echo "locations_api_url = \"http://127.0.0.1:$(port_of locations prod)\""
            ;;
        assets)
            echo "people_api_url = \"http://127.0.0.1:$(port_of people "$env")\""
            # Device-insights projection fans out to three services.
            echo "catalog_api_url = \"http://127.0.0.1:$(port_of catalog "$env")\""
            echo "jobs_api_url = \"http://127.0.0.1:$(port_of jobs "$env")\""
            echo "inventory_api_url = \"http://127.0.0.1:$(port_of inventory "$env")\""
            # Hard-requires the Class registry: asset events validate their
            # intake-source / warranty-coverage / condition at ingest.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        catalog)
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            # Hard-requires the Class registry: the marketing-asset
            # `kind`, DeviceCategory, and document-kind CHECKs were
            # retired for registry-backed validation.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        inventory)
            # Warehouse-status projection fans out to three services.
            echo "jobs_api_url = \"http://127.0.0.1:$(port_of jobs "$env")\""
            echo "assets_api_url = \"http://127.0.0.1:$(port_of assets "$env")\""
            echo "shipping_api_url = \"http://127.0.0.1:$(port_of shipping "$env")\""
            # Hard-requires the Class registry: the discrepancy_kind
            # CHECK was retired for registry-backed validation.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        shipping)
            # Hard-requires the Class registry: the carrier CHECK was
            # retired for registry-backed validation (carrier is
            # identity-first optional, validated only when present).
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
        jobs)
            # boss-jobs-api seeds the executive-role cache at startup
            # so the escalation router filters recipients by tenant-
            # defined `metadata.is_executive` Class membership.
            echo "classes_api_url = \"http://127.0.0.1:$(port_of classes prod)\""
            ;;
    esac
}

# Emit the TOML config body for a solo (prod-only) service.
emit_solo_config() {
    local name="$1"
    local port
    port=$(port_of "$name" prod)
    case "$name" in
        sim)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
http_bind = "0.0.0.0:$port"
scratch_postgres_url = "$SCRATCH_DB_URL"
scratch_api_url = "scratch://127.0.0.1"
repo_root = "$REPO_ROOT"
EOF
            ;;
        ml)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
EOF
            ;;
        ledger)
            # NATS publisher gates the ledger.* upstream events the
            # audit_log → financial_facts projection relies
            # on. Without it the live writes still land but
            # rebuild_facts has no audit_log rows to project from.
            # classes_api_url seeds the executive-role cache for the
            # /api/it/providers admin gate.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"
classes_api_url = "http://127.0.0.1:$(port_of classes prod)"
EOF
            ;;
        content)
            # NATS publisher gates content.bulletin.* emission to
            # audit_log; without it bulletins write to the projection
            # but never enter the event stream.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"
EOF
            ;;
        clock)
            # boss-clock-api takes everything from env vars
            # (BOSS_CLOCK_MODE, BOSS_SIM_TICK_*, BOSS_POSTGRES_URL).
            # We emit a stub config so the uniform --config flag
            # stays happy; emit_unit() injects the env.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# (boss-clock-api takes all settings from env vars.)
port = $port
EOF
            ;;
        policy)
            # boss-policy-api reads its port from env; a config file isn't
            # required today but we emit a stub so deploy-services.sh's
            # uniform --config flag stays happy. The port-env injection
            # is handled in emit_unit().
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# (boss-policy-api takes its port from BOSS_POLICY_PORT env var.)
port = $port
EOF
            ;;
        docs)
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
repo_root = "$REPO_ROOT"
EOF
            ;;
        classes|locations|subject-kinds|events)
            # Read-only registry / read-surface services. Just need
            # a Postgres URL + bind. No NATS (no event publishing —
            # they're lookup tables / read-side mirrors), no
            # cross-service deps. boss-events-api falls under this
            # shape: it reads audit_log + emits no new events.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
EOF
            ;;
        accounts)
            # Accounts service: publisher (NATS) for account-team
            # changes + notes + risk events, Postgres for projection
            # state, classes lookups for account_team_role validation,
            # assets for open-ticket-count on the account detail view.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"
classes_api_url = "http://127.0.0.1:$(port_of classes prod)"
assets_api_url = "http://127.0.0.1:$(port_of assets prod)"
EOF
            ;;
        products)
            # Finished-product catalog + per-location on-hand. NATS
            # publisher gates the products.* state events the
            # rebuild path (and downstream /shop / /products UI)
            # consume.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
postgres_url = "$PROD_DB_URL"
http_bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"
EOF
            ;;
        dispatcher)
            # boss-dispatcher reads its config from env vars (see
            # DispatcherConfig::default in crates/core/boss-dispatcher).
            # The emit_unit special case below pipes the same values
            # in as Environment= directives. This TOML stub exists
            # only so the deploy plan check has a path to diff
            # against — the binary doesn't open it.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# boss-dispatcher is env-var driven; see the systemd unit for the
# canonical values.
EOF
            ;;
        observability)
            # Cross-VM Cybernetics aggregator. Single-VM v1 deploys
            # leave [[vms]] empty and rely on [demo_agents] to populate
            # /api/snapshot with synthetic agent telemetry; the SPA
            # renders an honest "demo mode" banner on /ops in that
            # case. Multi-VM deploys would set [[vms]] entries and
            # remove [demo_agents] — out of scope for v1 single-VM.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
bind = "0.0.0.0:$port"
nats_url = "$NATS_URL"

[demo_agents]
tick_seconds = 8
EOF
            ;;
        simulator)
            # boss-simulator is env-var driven (BOSS_SIM_BIND /
            # BOSS_SIM_STATIC_DIR / BOSS_JOBS_URL / BOSS_CLOCK_URL); see
            # the systemd unit. This stub keeps the config path uniform.
            cat <<EOF
# Managed by infra/deploy-services.sh — edits will be overwritten.
# boss-simulator is env-var driven; see the systemd unit.
EOF
            ;;
        *)
            echo "emit_solo_config: unknown service '$name'" >&2
            return 1
            ;;
    esac
}

# Service-name shape. Most boss services ship as `boss-<name>-api`
# with config at /etc/boss-<name>-api.toml — but a handful (today:
# observability, which is a NATS aggregator + dashboard backend
# rather than a CRUD API) ship without the `-api` suffix. Centralise
# the mapping so unit names, config paths, binary names, and health-
# probe URLs all stay in sync.
stem_for() {
    case "$1" in
        observability) echo "boss-observability" ;;
        dispatcher)    echo "boss-dispatcher" ;;
        simulator)     echo "boss-simulator" ;;
        *)             echo "boss-$1-api" ;;
    esac
}

# Emit the systemd unit body for a service in a given env.
# `kind` is "paired" or "solo"; `env` is "prod" or "scratch".
emit_unit() {
    local kind="$1" name="$2" env="$3"
    local unit_name desc cfg_path after working_dir=""
    local label
    local stem
    stem=$(stem_for "$name")
    if [[ "$env" == "scratch" ]]; then
        unit_name="${stem}-scratch.service"
        cfg_path="/etc/${stem}-scratch.toml"
        label=" (scratch stack)"
    else
        unit_name="${stem}.service"
        cfg_path="/etc/${stem}.toml"
        label=""
    fi

    desc="Boss $(description_of "$name")${label}"

    # After= ordering. Paired services all use NATS + Postgres.
    # Solo services use Postgres only (ml, docs, sim).
    after="network-online.target postgresql.service"
    if [[ "$kind" == "paired" ]]; then
        after="$after nats-server.service"
    fi

    # sim and docs need the repo as CWD so they can read local files.
    if [[ "$name" == "sim" || "$name" == "docs" ]]; then
        working_dir="WorkingDirectory=$REPO_ROOT"
    fi

    # Per-service environment injection. Policy takes its port from env;
    # the config file is a stub for uniformity.
    local extra_env=""
    if [[ "$name" == "policy" ]]; then
        extra_env="Environment=BOSS_POLICY_PORT=$(port_of policy prod)"
    elif [[ "$name" == "clock" ]]; then
        # boss-clock-api reads everything from env vars; the
        # deploy emits wall mode by default. Operators flip a
        # demo deploy to sim via
        # `sudo systemctl edit boss-clock-api` adding
        # `Environment=BOSS_CLOCK_MODE=sim` +
        # `BOSS_SIM_TICK_INTERVAL_MS=10000` for the canonical
        # 1-day-per-10-wall-sec demo cadence.
        extra_env=$'Environment=BOSS_CLOCK_MODE=wall\nEnvironment=BOSS_SIM_TICK_SIZE_SECONDS=86400\nEnvironment=BOSS_SIM_TICK_INTERVAL_MS=0\nEnvironment=BOSS_POSTGRES_URL='"$PROD_DB_URL"
    elif [[ "$name" == "dispatcher" ]]; then
        # boss-dispatcher reads everything from env vars: URLs for every
        # downstream API its handlers post into (commerce/products/
        # shipping/ledger/inventory/people/jobs), plus the Postgres URL —
        # it loads its rule registry from the `dispatcher_rules` table.
        extra_env=$'Environment=BOSS_NATS_URL='"$NATS_URL"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_JOBS_URL=http://127.0.0.1:'"$(port_of jobs prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_PEOPLE_URL=http://127.0.0.1:'"$(port_of people prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_INVENTORY_URL=http://127.0.0.1:'"$(port_of inventory prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_COMMERCE_URL=http://127.0.0.1:'"$(port_of commerce prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_PRODUCTS_URL=http://127.0.0.1:'"$(port_of products prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_SHIPPING_URL=http://127.0.0.1:'"$(port_of shipping prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_LEDGER_URL=http://127.0.0.1:'"$(port_of ledger prod)"
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_POSTGRES_URL='"$PROD_DB_URL"
        # Step-assignment distribution strategy, named as explicit data
        # rather than left to the binary's default. `spread` fans each
        # ready step across its role's active holders by a stable hash of
        # the step id (deterministic); `lowest-id` is the legacy single-
        # holder behavior. Unknown value → dispatcher warns + falls back to
        # spread, so a typo here can't take assignment down.
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_DISPATCH_STRATEGY=spread'
        # Drives the collections loop: the webhook.notify rule pushes
        # step.done.billing here, where the brewery-sim's callback receiver
        # (BOSS_SIM_CALLBACK_BIND=127.0.0.1:7099) consumes it. Without this the
        # counterparty stays dark and zero collections fire (AR balloons).
        extra_env=$"${extra_env}"$'\nEnvironment=BOSS_EVENT_WEBHOOK_URL=http://127.0.0.1:7099/callback'
    elif [[ "$name" == "simulator" ]]; then
        # boss-simulator serves the apps/simulator bundle from this dir
        # (installed by infra/deploy-simulator-web.sh); BOSS_SIM_BIND +
        # the jobs/clock URLs fall back to boss_ports defaults.
        extra_env="Environment=BOSS_SIM_STATIC_DIR=/var/lib/boss-simulator/dist"
    fi

    # Env-driven services don't take a --config flag.
    local exec_start
    if [[ "$name" == "clock" || "$name" == "dispatcher" || "$name" == "simulator" ]]; then
        exec_start="ExecStart=/usr/local/bin/${stem}"
    else
        exec_start="ExecStart=/usr/local/bin/${stem} --config $cfg_path"
    fi

    cat <<EOF
[Unit]
Description=$desc
After=$after
Wants=network-online.target

[Service]
Type=simple
Environment=RUST_LOG=info
${extra_env:+$extra_env
}${exec_start}
${working_dir:+$working_dir
}Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
}

# ---------------------------------------------------------------------------
# Plan / apply
# ---------------------------------------------------------------------------

plan_one() {
    local kind="$1" name="$2" env="$3"
    local cfg_body unit_body cfg_path unit_path
    local stem
    stem=$(stem_for "$name")

    if [[ "$kind" == "paired" ]]; then
        cfg_body=$(emit_paired_config "$name" "$env")
    else
        cfg_body=$(emit_solo_config "$name")
    fi
    unit_body=$(emit_unit "$kind" "$name" "$env")

    if [[ "$env" == "scratch" ]]; then
        cfg_path="/etc/${stem}-scratch.toml"
        unit_path="/etc/systemd/system/${stem}-scratch.service"
    else
        cfg_path="/etc/${stem}.toml"
        unit_path="/etc/systemd/system/${stem}.service"
    fi

    if [[ "$MODE" == "check" ]]; then
        diff_file "$cfg_path" "$cfg_body"
        diff_file "$unit_path" "$unit_body"
        return 0
    fi

    printf '%s\n' "$cfg_body"  > "$cfg_path"
    printf '%s\n' "$unit_body" > "$unit_path"
    echo "  wrote $cfg_path"
    echo "  wrote $unit_path"
}

diff_file() {
    local path="$1" expected="$2"
    if [[ ! -f "$path" ]]; then
        echo "MISSING  $path"
        return 0
    fi
    local actual
    actual=$(cat "$path")
    if [[ "$actual" == "$expected" ]]; then
        echo "ok       $path"
    else
        echo "DIFF     $path"
        diff -u <(echo "$actual") <(echo "$expected") | sed 's/^/    /' || true
    fi
}

run_env() {
    local env="$1"
    echo "==> ${MODE} env=$env"
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r name _ _ <<<"$entry"
        plan_one paired "$name" "$env"
    done
    if [[ "$env" == "prod" ]]; then
        for entry in "${SOLO_SERVICES[@]}"; do
            IFS=: read -r name _ <<<"$entry"
            plan_one solo "$name" prod
        done
    fi
}

if [[ "$TARGET" == "both" ]]; then
    run_env prod
    run_env scratch
else
    run_env "$TARGET"
fi

if [[ "$MODE" == "check" ]]; then
    exit 0
fi

echo "==> systemctl daemon-reload"
systemctl daemon-reload

# boss-dispatcher's rule registry now lives in the `dispatcher_rules`
# table (seeded by 41-dispatcher.sql, authored from infra/dispatcher/
# rules.toml via gen-seed.py) and is loaded at startup — no rules.toml
# file deploy.

# Resolve target dir — may be a symlink (`/opt/boss/target` →
# `/var/lib/boss-build/target` per `infra/dev-bootstrap`) or a real
# directory (fresh checkout, or after a disk-cleanup that removed
# the symlink). `readlink -f` collapses both.
TARGET_DIR=$(readlink -f "$REPO_ROOT/target")
RELEASE_DIR="$TARGET_DIR/release"

# CANONICAL BUILD: infra/build-release.sh — run it before this script.
# It reads each bin's `required-features` straight from `cargo metadata`
# and builds every gated bin (postgres, plus per-service `<svc>-api`
# umbrella features like accounts-api / events-api). A plain
# `cargo build --release --workspace` instead leaves *in-memory* binaries
# for the many `default = []` service crates — they boot, then exit 1, or
# silently serve a store that loses every write. There is deliberately no
# hand-maintained feature list here: the old one drifted to 7 of the 37
# gated bins and silently shipped stale/in-memory binaries every deploy.
#
# Freshness-guard input: the newest tracked-source mtime. `install_binary`
# refuses to install any binary older than this — catching the "stale
# binary" footgun (deploying yesterday's build) at deploy time, not via a
# 000 health probe after the damage is done.
# awk (not `head -1 | cut`) reads the whole stream — piping into a
# truncating `head` SIGPIPEs `sort`/`find`, and under `set -o pipefail`
# that 141 aborts the deploy before it installs anything.
NEWEST_SRC_MTIME=$(find "$REPO_ROOT/crates" \( -name '*.rs' -o -name 'Cargo.toml' \) -printf '%T@\n' 2>/dev/null | sort -rn | awk 'NR==1{print int($1)}')

echo "==> install fresh binaries from $RELEASE_DIR"
echo "    (build first with: ./infra/build-release.sh)"
declare -a TO_DEPLOY=()
add_units_for_env() {
    local env="$1"
    local stem
    for entry in "${PAIRED_SERVICES[@]}"; do
        IFS=: read -r name _ _ <<<"$entry"
        stem=$(stem_for "$name")
        if [[ "$env" == "scratch" ]]; then
            TO_DEPLOY+=("${stem}-scratch.service")
        else
            TO_DEPLOY+=("${stem}.service")
        fi
    done
    if [[ "$env" == "prod" ]]; then
        for entry in "${SOLO_SERVICES[@]}"; do
            IFS=: read -r name _ <<<"$entry"
            stem=$(stem_for "$name")
            TO_DEPLOY+=("${stem}.service")
        done
    fi
}
if [[ "$TARGET" == "both" ]]; then
    add_units_for_env prod
    add_units_for_env scratch
else
    add_units_for_env "$TARGET"
fi

# Install fresh binaries before restarting. `install -m 0755` is
# atomic write+rename, which works even when the target is currently
# executing a service (`cp` errors with "Text file busy" because
# Restart=on-failure brings the service back up too quickly between
# stop and the cp call). Skips silently when the source binary
# isn't built — useful for partial deploys after a focused
# `cargo build -p <one-service>`.
install_binary() {
    local bin_name="$1"
    local src="$RELEASE_DIR/$bin_name"
    local dst="/usr/local/bin/$bin_name"
    if [[ ! -f "$src" ]]; then
        echo "  SKIP $bin_name (not built at $src)"
        return 0
    fi
    # Freshness guard: a binary older than the newest source is a stale
    # build — installing it ships old code (or, for a `default=[]` crate
    # never rebuilt with --features, an in-memory binary). Refuse loudly;
    # the fix is always `./infra/build-release.sh`.
    if [[ -n "${NEWEST_SRC_MTIME:-}" ]]; then
        local bin_mtime
        bin_mtime=$(stat -c '%Y' "$src" 2>/dev/null || echo 0)
        if [[ "$bin_mtime" -lt "$NEWEST_SRC_MTIME" ]]; then
            echo "  STALE $bin_name ($(date -d "@$bin_mtime" '+%F %H:%M') < newest source) — rebuild via ./infra/build-release.sh" >&2
            exit 1
        fi
    fi
    install -m 0755 "$src" "$dst"
    echo "  installed $bin_name"
}
for unit in "${TO_DEPLOY[@]}"; do
    # boss-shipping-api.service → boss-shipping-api;
    # boss-shipping-api-scratch.service → boss-shipping-api
    # (scratch units run the same binary against a different config).
    bin_name="${unit%.service}"
    bin_name="${bin_name%-scratch}"
    install_binary "$bin_name"
done

echo "==> restart services"
for unit in "${TO_DEPLOY[@]}"; do
    systemctl enable "$unit" >/dev/null 2>&1 || true
    systemctl restart "$unit"
    echo "  restarted $unit"
done

# Daemons + timers are environment-agnostic — they always land in
# prod. Skip on `scratch`-only deploys to avoid stomping prod state
# from a scratch run.
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
    # On-demand helper binaries with no systemd unit of their own — a
    # running service shells out to them. boss-jobs-api's demo-loop Reset
    # spawns boss-rebuild-all to re-derive projections after trimming
    # audit_log; left out of every install list, it silently rotted to a
    # pre-migration build and Reset failed mid-rebuild. Freshness-guarded
    # like every other binary, so a stale one now fails the deploy loudly.
    echo "==> install on-demand helper binaries"
    install_binary "boss-rebuild-all"

    # boss-gateway + boss-brewery-sim aren't port-table *-api services and have
    # no DAEMONS entry, so no install list refreshed them — they rotted to
    # pre-migration builds the same way boss-rebuild-all did. Refresh +
    # freshness-guard the binaries and bounce them if running (the gateway
    # always; the sim only in a demo deployment, where its ExecStartPre gate
    # keeps it up). Their unit files are managed out-of-band.
    echo "==> refresh non-port-table service binaries (gateway, brewery-sim)"
    for bin in boss-gateway boss-brewery-sim; do
        install_binary "$bin"
        if systemctl is-active --quiet "$bin" 2>/dev/null; then
            systemctl restart "$bin" && echo "  restarted $bin"
        fi
    done

    echo "==> install + enable daemons"
    for entry in "${DAEMONS[@]}"; do
        IFS=: read -r stem subdir <<<"$entry"
        src_dir="$REPO_ROOT/infra"
        [[ "$subdir" != "." ]] && src_dir="$src_dir/$subdir"
        svc_src="$src_dir/${stem}.service"
        if [[ ! -f "$svc_src" ]]; then
            echo "  SKIP $stem (missing $svc_src)"
            continue
        fi
        install -m 0644 "$svc_src" "/etc/systemd/system/${stem}.service"
        install_binary "$stem"
        echo "  installed $stem unit + binary"
    done
    systemctl daemon-reload
    for entry in "${DAEMONS[@]}"; do
        IFS=: read -r stem _ <<<"$entry"
        if [[ -f "/etc/systemd/system/${stem}.service" ]]; then
            systemctl enable "${stem}.service" >/dev/null 2>&1 || true
            systemctl restart "${stem}.service"
            echo "  restarted ${stem}.service"
        fi
    done

    echo "==> install + enable timers"
    for entry in "${TIMERS[@]}"; do
        IFS=: read -r stem subdir <<<"$entry"
        src_dir="$REPO_ROOT/infra"
        [[ "$subdir" != "." ]] && src_dir="$src_dir/$subdir"
        svc_src="$src_dir/${stem}.service"
        tmr_src="$src_dir/${stem}.timer"
        if [[ ! -f "$svc_src" || ! -f "$tmr_src" ]]; then
            echo "  SKIP $stem (missing $svc_src or $tmr_src)"
            continue
        fi
        install -m 0644 "$svc_src" "/etc/systemd/system/${stem}.service"
        install -m 0644 "$tmr_src" "/etc/systemd/system/${stem}.timer"
        # Install the timer's binary if it's been built. The unit
        # stem matches the binary name (e.g. boss-ledger-recognize).
        install_binary "$stem"
        echo "  installed $stem unit + binary"
    done
    systemctl daemon-reload
    for entry in "${TIMERS[@]}"; do
        IFS=: read -r stem _ <<<"$entry"
        if [[ -f "/etc/systemd/system/${stem}.timer" ]]; then
            systemctl enable --now "${stem}.timer" >/dev/null 2>&1 || true
            echo "  enabled ${stem}.timer"
        fi
    done
fi

echo "==> health probes"
sleep 1
probe_one() {
    local kind="$1" name="$2" env="$3"
    local port
    port=$(port_of "$name" "$env")
    # Most services mount routes under /api/<name>; boss-docs-api is
    # named "docs" internally but serves at /api/design/*; boss-
    # observability is a non-`-api` service that mounts /api/health
    # directly (no per-service prefix) — it's a NATS aggregator, not
    # a domain-CRUD service.
    local url
    case "$name" in
        docs)          url="http://127.0.0.1:${port}/api/design/health" ;;
        observability) url="http://127.0.0.1:${port}/api/health" ;;
        *)             url="http://127.0.0.1:${port}/api/${name}/health" ;;
    esac
    local code
    code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 "$url" || echo "???")
    local label="${name}-${env}"
    printf '  %-20s  GET %-45s  %s\n' "$label" "$url" "$code"
}
for entry in "${PAIRED_SERVICES[@]}"; do
    IFS=: read -r name _ _ <<<"$entry"
    if [[ "$TARGET" == "prod"  || "$TARGET" == "both" ]]; then probe_one paired "$name" prod; fi
    if [[ "$TARGET" == "scratch" || "$TARGET" == "both" ]]; then probe_one paired "$name" scratch; fi
done
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]]; then
    for entry in "${SOLO_SERVICES[@]}"; do
        IFS=: read -r name _ <<<"$entry"
        probe_one solo "$name" prod
    done
fi

# Front door. deploy-services manages the API services but NOT the
# gateway (nor the sim) — so a `systemctl stop boss-*` + deploy-services
# reset would leave the gateway down and caddy would serve the "demo
# regenerating" splash even though every API above is healthy (this bit
# us 2026-06-19). Ensure + verify the gateway here so the bring-up can't
# silently leave the public face down.
if [[ "$TARGET" == "prod" || "$TARGET" == "both" ]] \
    && systemctl list-unit-files boss-gateway.service >/dev/null 2>&1; then
    if ! systemctl is-active --quiet boss-gateway.service; then
        echo "==> gateway not running — starting it (front door; not in the managed list)"
        systemctl enable --now boss-gateway.service >/dev/null 2>&1 || true
        systemctl start boss-gateway.service || true
        sleep 2
    fi
    gw_code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 http://127.0.0.1:4443/ || echo "000")
    if [[ "$gw_code" =~ ^(200|302|401|403)$ ]]; then
        printf '  %-20s  GET %-45s  %s\n' "gateway(front-door)" "http://127.0.0.1:4443/" "$gw_code"
    else
        echo "  !! GATEWAY NOT RESPONDING ($gw_code) — the public face will show the" >&2
        echo "     'demo regenerating' splash until :4443 is up. Investigate boss-gateway." >&2
    fi
    # The sim (also unmanaged here) gates on BOSS_DEMO_MODE; flag it if a
    # demo host left it down, but don't force-start (non-demo deploys
    # intentionally leave it off).
    if systemctl is-enabled --quiet boss-brewery-sim.service 2>/dev/null \
        && ! systemctl is-active --quiet boss-brewery-sim.service; then
        echo "  note: boss-brewery-sim is enabled but not active — start it if this is a demo host."
    fi
fi

echo "done."
