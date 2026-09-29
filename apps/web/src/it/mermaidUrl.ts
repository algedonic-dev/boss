// Where the browser loads Mermaid from, for /it/kb only (backlog
// 4718d918). One definition, read by the page (KbDiagram.svelte), the
// build that copies the files (build.ts via mermaid-vendor.ts) and the
// dev-server that serves them in development (dev-server.ts).
//
// A root path, because the gateway serves the SPA's dist at `/` as well
// as `/dashboard/` (boss-gateway static_files.rs). The VERSION is in the
// path because the gateway marks every non-index file `immutable` for a
// year, and the entry's name does not change between Mermaid releases:
// without it, a browser would keep the old entry after an upgrade and
// ask for chunk files the new build no longer has.

import { version as MERMAID_VERSION } from 'mermaid/package.json';

export const MERMAID_ROUTE = `/vendor/mermaid-${MERMAID_VERSION}/`;
/** Mermaid's self-contained ESM build; its chunks sit in chunks/mermaid.esm.min/. */
export const MERMAID_ENTRY = 'mermaid.esm.min.mjs';
