---
layout: home
title: BOSS — Beer Open Source Software for System Modeling
---

# BOSS

Event-sourced software for modeling systems as state machines.
Named in tribute to **Stafford Beer**, the British cybernetician
whose work on the Viable System Model and Project Cybersyn
shaped how we think about modeling organizations in software.

The full thesis, founding ideas, and core design principles live
canonical in [CLAUDE.md](https://github.com/algedonic-dev/boss/blob/main/CLAUDE.md)
— the project's coding-and-design rulebook. This page is the
documentation directory.

## Start here

| If you want to… | Read |
| --- | --- |
| Understand what BOSS *is* | [CLAUDE.md §Project Overview + §Founding ideas](https://github.com/algedonic-dev/boss/blob/main/CLAUDE.md#project-overview) |
| See the four primitives + how tenants extend them | [Extending BOSS](design/extending-boss.md) |
| Read the correctness thesis | [The BOSS correctness protocol](design/correctness-protocol.md) |
| See the architecture | [Architecture diagrams](architecture-diagram.md) |
| Read the decision record | [Baseline Architecture Decisions](architecture-decisions.md) |
| Run BOSS locally | The repo's [Quick start](https://github.com/algedonic-dev/boss#quick-start) |

## Subdirectory map

- **[design/](design/)** — living design references (correctness,
  testing, the human-powered state-machine frame, extending, plus
  the registry patterns) and in-flight design work. Settled
  decisions fold into
  [`architecture-decisions.md`](architecture-decisions.md) each
  release.
- **[runbooks/](runbooks/)** — operator + developer runbooks.
- **[architecture/](architecture/)** — Mermaid sources + rendered
  SVG / PNG for the four architecture diagrams. The app's IT
  Knowledge Base page (`/system/kb`) imports these SVGs to render them
  in-app.
- **[formal/](formal/)** — TLA+ specs (`StepStatus.tla` for the
  Step lifecycle, `LedgerPeriodLock.tla` for journal posting +
  period locks) that the TLC model checker exhaustively verifies on
  a bounded model. (Kani proofs are not here; they live in the
  `boss-ledger` crate.)
