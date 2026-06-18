---
name: Bug report
about: Something is broken in BOSS
title: "[bug] "
labels: bug
---

## What happened

<!-- One-paragraph description of the unexpected behavior. -->

## Reproduction

<!-- The exact command, HTTP request, or UI flow that triggers
the bug. Reproductions on the brewery playground tenant are
strongly preferred — copy-paste-able commands save us hours. -->

## Expected behavior

<!-- What did you expect to happen instead? -->

## Environment

- BOSS version: <!-- `git rev-parse HEAD` or release tag -->
- Component: <!-- gateway / boss-jobs-api / SPA / brewery-engine / etc. -->
- OS: <!-- Linux distro + version, macOS, etc. -->
- Browser (for SPA bugs): <!-- Chrome 124, Firefox 125, … -->

## Logs / output

```
<!-- relevant `journalctl -u boss-<service>` output, or
browser-console errors. Trim secrets. -->
```

## Anything else?

<!-- Workarounds you've tried, related issues, hunches about
which crate is implicated. -->
