# Design: Kanidm IDM — the front door for real people (and agents)

**Status:** draft — open questions tracked at `/system/design`
**Origin:** David's direction (2026-08-10): Kanidm runs on the GCP box
and provides IDM to the Playground on the local cluster. Item
`98816e6a`.
**Related**: [dev-cluster.md](./dev-cluster.md) ·
[payload-encryption.md](./payload-encryption.md) (the other half of
"real people arrive")

## Why now, and why Kanidm

The sim-first strategy gates connecting real people on having the
model's kinks worked out. The other gate is mechanical: real people
need a front door, and header-injected claims from `credentials.toml`
are a bootstrap tool, not one. Kanidm fits the house style: Rust,
single binary, passkey-first, a real OIDC provider, and its own
state — no external database.

Topology: Kanidm lives on the GCP box (stable public IP, survives
cluster churn — the "admin seat in the sky" role), serving the
Playground wherever it runs. The cluster is a *client* of identity,
never its host: rebuilding the cluster must not lose the company's
logins.

## The shape

```
person/agent ──(OIDC auth-code, passkey)──► Kanidm (GCP box)
                                              │ id_token: sub, email, groups
                                              ▼
                      boss-gateway (OIDC client, session issuer)
                                              │ maps → EXISTING employee Subject (by email)
                                              │ maps groups → BOSS roles (registry)
                                              ▼
                                x-boss-user claims, as today
```

Two invariants make this BOSS-shaped rather than bolted on:

1. **Kanidm authenticates; it never provisions.** A login maps to an
   *existing* employee Subject or it fails closed. People enter the
   company through the People domain (hiring is a Workflow with an
   audit trail), not as a side effect of first login. The IdP must
   not become a second source of truth for who works here.
2. **Group→role mapping is a registry, not gateway code.** Kanidm
   owns membership; BOSS policy stays BOSS's. The join between them
   is data (`idp_group_roles` or kin), so IT manages access in Kanidm
   and the policy engine never learns Kanidm exists.

Local auth (`credentials.toml`) survives as break-glass: an IdP
outage must not lock the operators out of the system that runs the
company. The migration plan is untouched — the move happens on local
auth; OIDC lands after.

## Open questions

### Q1: Does the gateway hold the session, or does every request carry the token?

Today the gateway issues its own session after local auth. Keeping
that (gateway session, OIDC only at login) is the small change and
keeps every downstream service untouched. The alternative — services
validating bearer tokens themselves — buys per-service revocation at
the cost of every service growing an OIDC dependency. Proposed:
**gateway session**, revisit only if service-to-service auth needs it.

### Q2: What is the employee-mapping key, and what happens on a miss?

Email is the obvious join (Kanidm account email ↔ employee email).
A login with no matching employee: fail closed with a message, or
land in a "pending access" surface an admin can act on? Proposed:
**fail closed + audit event**; a pending-access Job is a nice later
step (the Job model doing IdP onboarding) but not v1.

### Q3: Do agents get Kanidm service accounts?

The executor model says agents are CPUs in the same machine. Today
agent identity is a forged claim header on a trusted box. Kanidm
service accounts (API tokens, real group membership) would make agent
identity honest and revocable — and make the audit log's actor claims
independently verifiable against the IdP. Cost: every agent caller
grows a token flow. Proposed: **yes, but phase 2** — humans first,
agents while the header path still works, then the header path dies.

### Q4: Where does Kanidm's own state live in the backup/migration story?

Kanidm's DB is the second member of the outside-git-and-Postgres
class (with `credentials.toml`). Its loss means every real person's
credentials and passkeys vanish. Proposed: its backup rides the
existing `backup.sh` timer (kanidm has an online backup facility),
and dev-cluster.md's copy-set section gains the pointer — the GCP
box is now stateful in one more way the cluster is not.

### Q5: DNS and TLS shape?

Kanidm terminates its own TLS and historically rejects
TLS-stripping proxies. Proposed: `id.algedonic.dev`, DNS-only
(grey-cloud) A record to the GCP box, Kanidm's own cert via its ACME
support or certbot — verify against current Kanidm docs at install
time. The gateway's OIDC callback stays behind the existing
Cloudflare front.
