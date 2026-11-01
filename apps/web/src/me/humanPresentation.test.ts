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

test('unspecified human classification is not invented from an assignment', () => {
  expect(humanPresentation(null).kind).toBe('other');
  expect(humanPresentation({ human_only: false }).kind).toBe('other');
});

test('evidence cannot inject an external or executable URL', () => {
  for (const evidence of ['javascript:alert(1)', 'https://elsewhere.invalid', '/invented']) {
    expect(humanPresentation({ human_only: true, human_presentation: { ...presentation, evidence } }).kind).toBe('missing');
  }
});
