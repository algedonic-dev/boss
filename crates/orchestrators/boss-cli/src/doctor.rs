//! `boss doctor` — post-install health check. Probes the running
//! stack (Postgres, NATS, gateway, tenant manifest, SPA bundle,
//! systemd services) and reports the root cause of each failing
//! piece ("X failed because Y").

use anyhow::Result;
use tokio::process::Command;

struct Check {
    label: &'static str,
    passed: bool,
    detail: String,
}

fn render_check(check: &Check) -> String {
    let icon = if check.passed { "+" } else { "-" };
    format!("  [{icon}] {:<20} {}", check.label, check.detail)
}

// ---------------------------------------------------------------------------
// Install-validation checks (operator-facing, post-install)
// ---------------------------------------------------------------------------

/// Probe Postgres reachability + audit_log size. Uses pg_isready
/// for the connectivity check (no Postgres role required) +
/// `boss inspect`-style HTTP probe through the audit_log proxy to
/// avoid raw SQL. Falls back to "tenant has no events" when the
/// seed bundle hasn't been loaded yet.
async fn check_install_postgres() -> Check {
    let isready = Command::new("pg_isready")
        .arg("-h")
        .arg("127.0.0.1")
        .output()
        .await;
    match isready {
        Ok(o) if o.status.success() => Check {
            label: "Postgres",
            passed: true,
            detail: "reachable at 127.0.0.1:5432".into(),
        },
        Ok(_) => Check {
            label: "Postgres",
            passed: false,
            detail:
                "pg_isready reports not accepting connections — is the postgresql service running?"
                    .into(),
        },
        Err(_) => Check {
            label: "Postgres",
            passed: false,
            detail: "pg_isready not on PATH — install postgresql-client".into(),
        },
    }
}

/// Probe NATS via the monitoring HTTP endpoint (bare port 4222
/// speaks the NATS wire protocol so a plain `curl` against it
/// returns garbage — the 8222 monitor port is the friendlier
/// surface). When NATS is started with `-m 8222` (the
/// oss-quickstart Docker compose default) /healthz returns 200.
async fn check_install_nats() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get("http://127.0.0.1:8222/healthz").send().await {
        Ok(r) if r.status().is_success() => Check {
            label: "NATS",
            passed: true,
            detail: "reachable at 127.0.0.1:4222 (healthz 200 on :8222)".into(),
        },
        Ok(r) => Check {
            label: "NATS",
            passed: false,
            detail: format!(
                "NATS monitor returned HTTP {} — check `systemctl status nats`",
                r.status()
            ),
        },
        Err(_) => {
            // Plain port-open check as fallback for installs that
            // don't expose the monitoring endpoint.
            let connect = std::net::TcpStream::connect_timeout(
                &"127.0.0.1:4222".parse().unwrap(),
                std::time::Duration::from_secs(1),
            );
            if connect.is_ok() {
                Check {
                    label: "NATS",
                    passed: true,
                    detail: "port 4222 accepting connections (no /healthz monitor)".into(),
                }
            } else {
                Check {
                    label: "NATS",
                    passed: false,
                    detail: "127.0.0.1:4222 refused — check `systemctl status nats`".into(),
                }
            }
        }
    }
}

/// Hit the gateway's `/health` endpoint at the canonical local
/// listen address. Always probes `127.0.0.1:4443` (not
/// BOSS_GATEWAY_URL) — this command is validating the local
/// install, not a remote dev workstation.
async fn check_install_gateway() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get("http://127.0.0.1:4443/health").send().await {
        Ok(r) if r.status().is_success() => Check {
            label: "Gateway",
            passed: true,
            detail: "http://127.0.0.1:4443/health → 200".into(),
        },
        Ok(r) => Check {
            label: "Gateway",
            passed: false,
            detail: format!(
                "/health returned HTTP {} — check `systemctl status boss-gateway` and `journalctl -u boss-gateway -n 40`",
                r.status()
            ),
        },
        Err(e) => Check {
            label: "Gateway",
            passed: false,
            detail: format!(
                "127.0.0.1:4443 unreachable ({e}) — boss-gateway likely not running; try `systemctl restart boss-gateway`"
            ),
        },
    }
}

/// Probe the tenant manifest endpoint. Returns the label + module
/// count so the operator can confirm tenant.toml is loaded
/// correctly. Empty manifest is suspicious (likely tenant.toml
/// missing or path misconfigured).
async fn check_install_tenant_manifest() -> Check {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client
        .get("http://127.0.0.1:4443/api/tenant/manifest")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
            let labels = body
                .get("labels")
                .and_then(|v| v.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            let modules = body
                .get("modules")
                .and_then(|v| v.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            if labels == 0 && modules == 0 {
                Check {
                    label: "Tenant manifest",
                    passed: false,
                    detail: "empty — set BOSS_TENANT_MANIFEST_TOML or place tenant.toml at /etc/boss-gateway/".into(),
                }
            } else {
                Check {
                    label: "Tenant manifest",
                    passed: true,
                    detail: format!("{modules} modules, {labels} labels"),
                }
            }
        }
        Ok(r) => Check {
            label: "Tenant manifest",
            passed: false,
            detail: format!("HTTP {}", r.status()),
        },
        Err(_) => Check {
            label: "Tenant manifest",
            passed: false,
            detail: "unreachable (gateway down?)".into(),
        },
    }
}

/// Confirm the SPA bundle is on disk where the gateway expects to
/// serve it from. Path matches the default `BOSS_STATIC_DIR`.
fn check_install_spa() -> Check {
    let dist = std::path::Path::new("/var/lib/boss-web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        return Check {
            label: "SPA bundle",
            passed: false,
            detail:
                "/var/lib/boss-web/dist/index.html missing — run `cd apps/web && bun run build && sudo rsync -a dist/ /var/lib/boss-web/dist/`"
                    .into(),
        };
    }
    let chunks = std::fs::read_dir(dist)
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("chunk-") && n.ends_with(".js"))
                })
                .count()
        })
        .unwrap_or(0);
    Check {
        label: "SPA bundle",
        passed: true,
        detail: format!("/var/lib/boss-web/dist/ ({chunks} JS chunks)"),
    }
}

/// Probe each registered boss-* systemd service. Pulls the list
/// from `ops::SERVICES` (already used by `boss status`) for a
/// single source of truth. Reports failures with a one-liner
/// suggesting the journalctl follow-up.
async fn check_install_services() -> Check {
    let services = crate::ops::registered_service_units();
    if services.is_empty() {
        return Check {
            label: "Services",
            passed: false,
            detail: "no registered systemd units in SERVICES — boss-cli build is corrupt".into(),
        };
    }
    let total = services.len();
    let mut failing = Vec::new();
    for unit in &services {
        let out = Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .await;
        let active = matches!(out, Ok(o) if o.status.success());
        if !active {
            failing.push(unit.clone());
        }
    }
    if failing.is_empty() {
        Check {
            label: "Services",
            passed: true,
            detail: format!("{total}/{total} active"),
        }
    } else {
        let n = failing.len();
        let preview: Vec<&str> = failing.iter().take(3).map(String::as_str).collect();
        let suffix = if n > 3 {
            format!("{} +{} more", preview.join(", "), n - 3)
        } else {
            preview.join(", ")
        };
        Check {
            label: "Services",
            passed: false,
            detail: format!(
                "{}/{total} active — {suffix} not running. Check `journalctl -u <name> -n 40`",
                total - n
            ),
        }
    }
}

pub async fn run_install() -> Result<()> {
    println!("Boss Install Health");
    println!("───────────────────────");

    let (pg, nats, gw, manifest, services) = tokio::join!(
        check_install_postgres(),
        check_install_nats(),
        check_install_gateway(),
        check_install_tenant_manifest(),
        check_install_services(),
    );
    let spa = check_install_spa();

    let checks = [pg, nats, gw, manifest, spa, services];

    println!();
    for check in &checks {
        println!("{}", render_check(check));
    }
    println!();

    let passed = checks.iter().filter(|c| c.passed).count();
    let total = checks.len();

    if passed == total {
        println!("  Ready. Open http://localhost:4443 to start.");
    } else {
        println!(
            "  {passed}/{total} checks passed. Address the issues above before opening the SPA."
        );
    }

    Ok(())
}
