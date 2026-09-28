import { describe, expect, test } from 'bun:test';

import { ruleHandlers, ruleProvenance } from './ruleProvenance';

const READ = { dir: '/opt/boss/infra/dispatcher/rules', rules: 62, error: null } as const;

// backlog f9e34a2c: the rules page painted a live-only product rule
// exactly like a reviewed tree rule. These pin the one mapping that
// tells them apart.
describe('ruleProvenance', () => {
  test('a product rule with a file is durable', () => {
    const p = ruleProvenance({ source: 'product', authored: true }, READ);
    expect(p).toEqual({
      label: 'file',
      kind: 'durable',
      why: 'authored under infra/dispatcher/rules/ — a restart keeps it',
    });
  });

  test('a product rule no file names is live only, and the next boot retires it', () => {
    const p = ruleProvenance({ source: 'product', authored: false }, READ);
    expect(p.label).toBe('live only');
    expect(p.kind).toBe('drift');
    expect(p.why).toContain('the dispatcher’s next boot retires it');
  });

  test('a tenant rule names its tenant and is not drift', () => {
    const p = ruleProvenance({ source: 'tenant:algedonic', authored: false }, READ);
    expect(p.label).toBe('tenant:algedonic');
    expect(p.kind).toBe('durable');
    expect(p.why).toContain('seeds/rules.toml');
  });

  test('unauthored is unknown, not drift, when the authored registry could not be read', () => {
    // authored:false is what every row says when BOSS_DISPATCHER_RULES
    // is unset — calling all of them live-only would be the confident
    // wrong answer http.rs the_response_says_why_the_whys_are_missing
    // exists to prevent.
    const unset = { dir: null, rules: 0, error: 'BOSS_DISPATCHER_RULES is not set' };
    expect(ruleProvenance({ source: 'product', authored: false }, unset).kind).toBe('unknown');
    expect(ruleProvenance({ source: 'product', authored: false }, null).kind).toBe('unknown');
    // A tenant rule never depended on the product's directory.
    expect(ruleProvenance({ source: 'tenant:acme', authored: false }, unset).kind).toBe('durable');
  });

  test('a row from a dispatcher that serves no source or authored flag is unknown', () => {
    const p = ruleProvenance({}, READ);
    expect(p.kind).toBe('unknown');
    expect(p.label).toBe('unknown');
  });
});

describe('ruleHandlers', () => {
  test('names each handler once, in do order', () => {
    expect(
      ruleHandlers({
        do: [
          { handler: 'jobs.spawn', args: {} },
          { handler: 'notify.assignee', args: {} },
          { handler: 'jobs.spawn', args: {} },
        ],
      }),
    ).toBe('jobs.spawn, notify.assignee');
  });

  test('an empty do list says so', () => {
    expect(ruleHandlers({ do: [] })).toBe('no handler');
  });
});
