// The resolution chain for the decision-context panel (19db52de):
// step's own context_md wins, then the job's, then the job's filed
// message — and blank strings are absences, not content.

import { describe, expect, test } from 'bun:test';
import { contextFromJob, contextFromStep } from './decisionContext';

describe('decision context resolution', () => {
  test('a step that carries its own context wins outright', () => {
    expect(contextFromStep({ context_md: 'approve the 62-commit push' })).toEqual({
      text: 'approve the 62-commit push',
      source: 'step',
    });
  });

  test('a blank or missing step context is an absence', () => {
    expect(contextFromStep({})).toBeNull();
    expect(contextFromStep({ context_md: '   ' })).toBeNull();
    expect(contextFromStep({ context_md: 42 })).toBeNull();
  });

  test('the job falls back context_md then message', () => {
    expect(
      contextFromJob({ context_md: 'briefing', message: 'filed text' }),
    ).toEqual({ text: 'briefing', source: 'job-context' });
    expect(contextFromJob({ message: 'filed text' })).toEqual({
      text: 'filed text',
      source: 'job-message',
    });
    expect(contextFromJob({})).toBeNull();
  });

  test('every user-feedback packet is self-presenting via its message', () => {
    // The 28 Decide-the-design steps in David's queue carry no
    // context_md anywhere; their case is the filed message. The chain
    // must surface it rather than render another empty form.
    const jobMeta = { message: 'My Day cannot say whether a packet needs me' };
    expect(contextFromJob(jobMeta)?.source).toBe('job-message');
  });
});
