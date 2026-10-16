import { expect, test } from 'bun:test';
import contract from './proof-strings.json';
import { rustTrim, nonBlankProofString, proofString, isWorldOwner } from './proof-strings';

test('Unicode White_Space is exactly the complete set pinned by actual Rust', () => {
  const actual: number[] = [];
  for (let code = 0; code <= 0x10ffff; code++) {
    if (code >= 0xd800 && code <= 0xdfff) continue;
    if (/\p{White_Space}/u.test(String.fromCodePoint(code))) actual.push(code);
  }
  expect(actual).toEqual(contract.white_space);
  for (const code of contract.white_space) {
    const character = String.fromCodePoint(code);
    expect(rustTrim(`${character}binding${character}`)).toBe('binding');
    expect(nonBlankProofString(character)).toBeNull();
  }
  for (const code of contract.non_white_space) {
    const character = String.fromCodePoint(code);
    expect(rustTrim(`${character}binding${character}`)).toBe(`${character}binding${character}`);
    expect(nonBlankProofString(character)).toBe(character);
  }
});

test('native string reads retain exact bindings and refuse non-string values', () => {
  for (const value of [undefined, null, true, 1, [], {}]) {
    expect(proofString(value)).toBeNull();
    expect(nonBlankProofString(value)).toBeNull();
  }
  expect(proofString(' \u0085observer\ufeff ')).toBe(' \u0085observer\ufeff ');
  expect(nonBlankProofString('\u0085\t observer \n\u3000')).toBe('observer');
  expect(nonBlankProofString('\ufeff observer \ufeff')).toBe('\ufeff observer \ufeff');
  for (const owner of ['world', 'WORLD', 'WoRlD']) expect(isWorldOwner(owner)).toBe(true);
  for (const owner of [' world', 'world\n', '\ufeffworld', 'wоrld', 'worlds']) expect(isWorldOwner(owner)).toBe(false);
});

test('server-qualified wait reader preserves Rust-nonblank strings without JS normalization', async () => {
  const { parseRecordedWait } = await import('./waits');
  const row = { packet: 'packet', owner: 'owner', condition_owner: 'world', title: 'title',
    condition: 'condition', next_check_at: '2026-10-15T11:00:00Z', evidence: { kind: 'observation' } };
  for (const key of ['owner', 'condition_owner', 'condition', 'next_check_at']) {
    const value = { ...row, [key]: '\ufeff' };
    expect(parseRecordedWait(value)).toEqual(value);
    expect(() => parseRecordedWait({ ...row, [key]: '\u0085' })).toThrow(`missing ${key}`);
  }
  expect(parseRecordedWait({ ...row, evidence: { kind: '\ufeff' } }).evidence.kind).toBe('\ufeff');
  expect(() => parseRecordedWait({ ...row, evidence: { kind: '\u0085' } })).toThrow('missing evidence kind');
});
