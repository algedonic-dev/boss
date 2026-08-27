import { describe, expect, test } from 'bun:test';
import { PROTOCOL_PALETTE, protocolHue } from './packet-card';

// The packet-card vocabulary moved here from apps/web/src/it/yard/yard.ts
// when the card was promoted to web-kit (feedback d69033dd: one card
// grammar for every queue surface). yard.ts re-exports these, so the
// definition still lives exactly once (CLAUDE.md §9a).
describe('protocolHue', () => {
  test('is stable, palette-bound, and distinguishes the pipeline kinds', () => {
    expect(protocolHue('ship-a-change')).toBe(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('ship-a-change'));
    expect(PROTOCOL_PALETTE).toContain(protocolHue('some-future-kind'));
    expect(protocolHue('ship-a-change')).not.toBe(protocolHue('pr-train'));
    expect(new Set(PROTOCOL_PALETTE).size).toBe(PROTOCOL_PALETTE.length);
  });
});
