// Whether a Class registry read worked, and why not (backlog e520c794).
// Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { classesLoadOf, classesStateOfAnswer, classesUrl } from './classes-read';

const URL = classesUrl('marketing-asset');

describe('classesStateOfAnswer — a refused or malformed answer is an error that says why', () => {
  test('the url names the subject kind it asks for', () => {
    expect(URL).toBe('/api/classes?subject_kind=marketing-asset');
  });

  test('a list is ready, with its rows', () => {
    const rows = [{ code: 'photo' }];
    const st = classesStateOfAnswer(URL, { ok: true, status: 200 }, rows);
    expect(st).toEqual({ kind: 'ready', rows: rows as never });
  });

  test('a refusal names the url and the status, in the words the page read lines use', () => {
    expect(classesStateOfAnswer(URL, { ok: false, status: 503 }, null)).toEqual({
      kind: 'error',
      error: '/api/classes?subject_kind=marketing-asset: HTTP 503',
    });
  });

  test('a 200 whose body is not a list is an error, not an empty registry', () => {
    expect(classesStateOfAnswer(URL, { ok: true, status: 200 }, { data: [], total: 0 })).toEqual({
      kind: 'error',
      error: '/api/classes?subject_kind=marketing-asset: the answer was not a list',
    });
  });
});

describe('classesLoadOf — the load state a page renders beside classesFor', () => {
  test('a kind nobody has asked for yet is loading, not ready: nothing has answered', () => {
    expect(classesLoadOf(undefined)).toEqual({ kind: 'loading' });
  });

  test('loading stays loading', () => {
    expect(classesLoadOf({ kind: 'loading' })).toEqual({ kind: 'loading' });
  });

  test('ready carries no rows — classesFor is the one reader of them', () => {
    expect(classesLoadOf({ kind: 'ready', rows: [] })).toEqual({ kind: 'ready' });
  });

  test('an error keeps its reason', () => {
    expect(classesLoadOf({ kind: 'error', error: `${URL}: HTTP 500` })).toEqual({
      kind: 'error',
      error: '/api/classes?subject_kind=marketing-asset: HTTP 500',
    });
  });
});
