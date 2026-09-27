// Who declared an enforced dispatcher rule, and whether it survives the
// next boot — the one question the /it/registry/rules table could not
// answer (backlog f9e34a2c). The read served `source` and `authored` on
// every row and the page rendered neither, so a product rule published
// live and never written down (the CLAUDE.md §9a drift `authored`
// exists to expose) painted exactly like a reviewed tree rule. Pure:
// row + registry block in, a label out.
//
// The boot seed (boss-dispatcher rules::seed) retires every active
// product rule no file under infra/dispatcher/rules/ names; a tenant's
// rule is `authored: false` by construction and never retired by it.

import type { AuthoredRegistry, DispatcherRule } from './types';

export type Provenance = Readonly<{
  /** The cell text. */
  label: string;
  /** `drift` = enforced now, retired at the next boot; `unknown` = the
   *  read cannot say (an older dispatcher, or an authored registry it
   *  could not read). */
  kind: 'durable' | 'drift' | 'unknown';
  /** The hover: what the label means. */
  why: string;
}>;

export function ruleProvenance(
  rule: Pick<DispatcherRule, 'source' | 'authored'>,
  registry: AuthoredRegistry | null,
): Provenance {
  const source = rule.source;
  if (source?.startsWith('tenant:')) {
    return {
      label: source,
      kind: 'durable',
      why: 'declared in the tenant’s own seeds/rules.toml — a converge never retires it',
    };
  }
  if (source !== 'product' || rule.authored === undefined) {
    return {
      label: 'unknown',
      kind: 'unknown',
      why: 'this dispatcher does not say who declared the rule',
    };
  }
  if (rule.authored) {
    return {
      label: 'file',
      kind: 'durable',
      why: 'authored under infra/dispatcher/rules/ — a restart keeps it',
    };
  }
  // authored:false is what EVERY product row says when the directory
  // could not be read (http.rs the_response_says_why_the_whys_are_missing),
  // so it is drift only when the directory was read.
  if (registry === null || registry.error !== null) {
    return {
      label: 'unknown',
      kind: 'unknown',
      why: 'the authored registry could not be read, so whether a file names this rule is unknown',
    };
  }
  return {
    label: 'live only',
    kind: 'drift',
    why: 'no file under infra/dispatcher/rules/ names this rule — it is enforced now, and the dispatcher’s next boot retires it',
  };
}

/** The handlers a rule fires, each once, in `do` order. Every live row
 *  has exactly one (65 of 65, measured 2026-09-23), so a count carried
 *  no information and the name does. */
export function ruleHandlers(rule: Pick<DispatcherRule, 'do'>): string {
  const names = [...new Set(rule.do.map((d) => d.handler))];
  return names.length > 0 ? names.join(', ') : 'no handler';
}
