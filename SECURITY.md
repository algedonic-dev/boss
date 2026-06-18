# Security Policy

BOSS is maintained by a single person on the side. There are no
SLAs, no formal coordinated-disclosure window, no embargoed-patch
process. What follows is what you can realistically expect.

## Reporting a vulnerability

Please don't open a public GitHub issue for something with security
impact — that exposes every BOSS deployment between the report and
the fix.

Email <security@algedonic.dev>. Include whatever you have:

- The commit (`git rev-parse HEAD`) or release tag you tested.
- Which component is affected — gateway, a specific service, the
  SPA, an example tenant, the audit-log layer.
- How to reproduce it. The brewery playground tenant
  (<https://playground.algedonic.dev>) is a safe surface for
  proofs of concept.
- Whether you think it's being actively exploited.

I'll read it. I'll do my best to confirm, fix, and credit you in
the release notes (or anonymously, if you prefer). Timeline depends
on how busy I am and how complex the fix is — assume "weeks, not
hours" and we'll both be pleasantly surprised when it's faster.

## What I take seriously

These are the things I'd actually drop other work for:

- Authentication / authorization bypass in `boss-gateway`,
  `boss-policy`, or any row-level policy gate — mis-handled
  `x-boss-user` headers, session-cookie forgery, CF Access JWT
  holes, policy rules that grant broader access than intended.
- Audit-log tampering paths. The chain-hash + REVOKE-protected
  schema is meant to make tampering detectable; a path that
  bypasses `boss-audit-integrity-check` is in scope. See
  [`docs/architecture-decisions.md`](docs/architecture-decisions.md)
  §Correctness protocol & the audit log.
- SQL injection or OS command injection in any service or CLI.
- Secret exposure — credentials in commits, session tokens in
  logs, secrets leaking via error responses.
- Cross-service trust violations — a service accepting
  `x-boss-user` from a non-trusted origin, or trusting
  unauthenticated input from another service.
- SPA-side XSS / CSRF / cookie scope mistakes.
- Denial of service with a trivial unauthenticated trigger.

Things I appreciate the report on but won't drop everything for:

- Self-XSS or attacks requiring a malicious browser extension.
- Session-expiry timing issues without a security impact.
- DoS that requires authenticated abuse from inside an operator's
  own deployment (the operator already has the credentials).
- Issues in third-party services BOSS integrates with (Cloudflare,
  Postgres, NATS) — report those upstream.
- Audit-integrity warnings for `id` gaps from rolled-back
  transactions — documented as suspicious-not-proof in the
  integrity-check output.

## Supported versions

Only `main` is supported. There are no backports.

## A note on AI-assisted code

A meaningful share of this codebase was drafted with AI
assistance. If you find a class of issue you suspect is "the LLM
slipping the same pattern in multiple places," tell me — pattern
reports help me sweep for siblings, and that kind of finding is
exactly the shape an outside reviewer is well-placed to catch.

Thanks for helping keep BOSS safe.
