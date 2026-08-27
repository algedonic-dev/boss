# Caddy front door — boss-gcp (`playground.algedonic.dev`)

The Caddyfile that runs on **boss-gcp**, the GCP VM that is the
public edge for the playground. It was hand-maintained at
`/etc/caddy/Caddyfile` and out of version control until now — the
same gap `infra/cluster/manifests/` closed for the cluster.

**This directory is the source of truth for that file. Hand-edits on
the host are drift.**

**What fronts what**

Nothing public resolves to this VM. Both hostnames arrive through a
Cloudflare Tunnel: `cloudflared` on boss-gcp dials Caddy over
loopback with the original SNI, Caddy matches on it, and proxies
over the WireGuard tunnel into the home cluster.

| hostname | Caddy sends it to | what that is |
|---|---|---|
| `playground.algedonic.dev` | `10.20.0.30:80` | the cluster BOSS gateway Service (`infra/cluster/manifests/boss.yaml`, Cilium LB-IPAM) |
| `id.algedonic.dev` | `https://10.20.0.31` | Kanidm, the identity provider — cluster-owned, not a BOSS manifest |

Three things in the file are load-bearing and easy to "tidy" into a
breakage; the file's own comments say so at each site:

- **The `handle_errors` page.** When the gateway is unreachable — a
  seed regen with the demo DB offline, a rolling deploy — Caddy
  serves the "service interrupted" page with an explicit **HTTP
  200**, not the upstream's 5xx. Cloudflare Access replaces an
  origin 5xx with its own error page, so the body only reaches a
  visitor on a 200. The HTML is a backtick raw string so its quotes
  and braces are not parsed as Caddyfile tokens.
- **`header_up Host {host}` on `id`.** Kanidm binds WebAuthn to
  `origin`. Rewrite Host to the backend address and passkey
  registration fails on an origin mismatch.
- **`tls_insecure_skip_verify` on `id`.** The backend presents the
  Cloudflare Origin CA cert, valid for the name but issued by a CA
  outside Caddy's trust store. The hop is WireGuard to an isolated
  VLAN — a trust-store gap, not an exposure.

**Secrets stay out of tree**

The file names cert *paths* only —
`/etc/caddy/certs/algedonic.{crt,key}`, the Cloudflare Origin CA
keypair for `*.algedonic.dev`. The key material lives on the host
(root-owned, mode 0600) and is not in this tree and must not be.
`infra/lint/no-secrets.sh` scans every tracked file on every run, so
this copy is already covered by the existing gate; it needs no lint
of its own, and there is deliberately no host-diffing check — CI has
no reliable route to boss-gcp.

**How to apply**

There is no automatic converge for this host — unlike the cluster
manifests, nothing on the forge fetches this tree and applies it.
Edit here, ship it through the train, then apply by hand:

```sh
sudo cp infra/gcp/caddy/Caddyfile /etc/caddy/Caddyfile
sudo caddy validate --adapter caddyfile --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Validate before reloading, always: `reload` on a bad config leaves
the old one running, but `restart` on one does not, and the
playground is the demo everyone is pointed at.

**Do not run `infra/caddy/setup.sh` on this host**

`infra/caddy/` is a *different* artifact: the generic reference
Caddyfile for the OSS quickstart, a single `{$BOSS_HOSTNAME}` vhost
proxying to a local gateway on `127.0.0.1:4443`, with Let's Encrypt
issuance. Its `setup.sh` does
`install .../infra/caddy/Caddyfile /etc/caddy/Caddyfile` — the same
path this file occupies. Running it on boss-gcp would silently
replace the front door, the tunnel wiring, and the error page. The
two configs live in separate directories for that reason: one is
what we ship to strangers, this one is what runs one specific host.

**Known wart**

The `handle_errors` comment still says "the gateway
(127.0.0.1:4443)" from when this file proxied to a local gateway.
The actual upstream has been `10.20.0.30:80` since the cluster move.
The copy here is a byte-exact mirror of the host as of 2026-08-13,
stale comment included, so that landing it changes nothing; fix the
wording on the next edit that ships with a real change.
