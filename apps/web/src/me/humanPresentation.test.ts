import { expect, test } from 'bun:test';
import { humanPresentation } from './humanPresentation';

const presentation = { ask: 'Authorize the receiver', why_person: 'Your passkey is required', action: 'Inspect and sign', after: 'The authorized delivery can proceed', evidence: 'current-step' } as const;

test('the canonical human flag requires a complete presentation, including legacy rows', () => {
  for (const human_only of [true, ' TRUE ']) {
    expect(humanPresentation({ human_only }).kind).toBe('missing');
    expect(humanPresentation({ human_only, human_presentation: presentation })).toEqual({ kind: 'ready', presentation });
    for (const field of Object.keys(presentation)) {
      for (const invalid of [null, '', '  ', 27]) {
        expect(humanPresentation({ human_only, human_presentation: { ...presentation, [field]: invalid } }).kind).toBe('missing');
      }
    }
  }
});

// A person-completed decision kept off passkey presence declares its
// ask without human_only: design-doc's review (A163052, 7501eb95). On
// 2026-11-11 one sat ready in David's up-for-grabs with its ask on the
// step, and My Day drew a bare packet card instead (bf47083d).
test('a declared presentation renders on a person-decided step that is not human_only', () => {
  expect(humanPresentation({ human_presentation: presentation })).toEqual({ kind: 'ready', presentation });
  for (const field of Object.keys(presentation)) {
    expect(humanPresentation({ human_presentation: { ...presentation, [field]: '  ' } }).kind).toBe('missing');
  }
  expect(humanPresentation({ human_presentation: null }).kind).toBe('other');
});

test('unspecified human classification is not invented from an assignment', () => {
  expect(humanPresentation(null).kind).toBe('other');
  expect(humanPresentation({ human_only: false }).kind).toBe('other');
});

test('evidence cannot inject an external or executable URL', () => {
  for (const evidence of ['javascript:alert(1)', 'https://elsewhere.invalid', '/invented']) {
    expect(humanPresentation({ human_only: true, human_presentation: { ...presentation, evidence } }).kind).toBe('missing');
  }
});
