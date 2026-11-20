import { describe, expect, it } from 'bun:test';
import { fetchNeedsYou, HELD_CARS_HREF, NEEDS_YOU_LIMIT, NEEDS_YOU_URL, needHref, needsYouBoard, parseNeedsYou } from './human-needs';

// THE NEEDS-YOU BOARD — the one row of the HUD frame /it keeps (R3 of
// the transit-map retirement, backlog 23a4b4d7): the human-required IT
// jobs, approvals and ops requests among them, whoever holds them
// (David, cef1f661). Moved verbatim from hud.test.ts when that frame
// retired.

const NOW = Date.parse('2026-09-27T12:00:04Z');

describe('NEEDS YOU — canonical human-required IT jobs', () => {
  const step = (over: Record<string, unknown> = {}) => ({ id: 's1', kind: 'sign-off', title: 'Approve', status: 'ready', human_required: true, ...over });
  const job = (over: Record<string, unknown> = {}) => ({ id: 'j1', kind: 'design-doc', title: 'Decide the design', status: 'open', priority: 'standard', opened_on: '2026-09-25', steps: [step()], ...over });
  const page = (data: unknown[], over: Record<string, unknown> = {}) => parseNeedsYou({ human_required_view: 1, data, total: data.length, limit: NEEDS_YOU_LIMIT, offset: 0, ...over });
  const answered = (result: ReturnType<typeof needsYouBoard>) => {
    if (result.kind === 'unread') throw new Error(result.why);
    return result;
  };
  const board = (data: unknown[], over: Record<string, unknown> = {}) => answered(needsYouBoard({ kind: 'ready', data: page(data, over) }, NOW));

  it('does not count an agent step merely because it shares the human role', () => {
    expect(board([job({ steps: [step({ human_required: false, agent_takes: true, authority_role: 'platform-admin' })] })]).count).toEqual({ kind: 'zero' });
  });
  it('includes human-required work whoever holds it, counting a job once with both step links', () => {
    const result = board([job({ steps: [step({ assignee_id: 'someone-else' }), step({ id: 's2', status: 'active', assignee_id: null })] })]);
    if (result.kind !== 'rows') throw new Error(result.kind);
    expect(result.count).toEqual({ kind: 'value', text: '1', troubled: false });
    expect(result.lines.map((line) => line.id)).toEqual(['s1', 's2']);
    expect(result.lines[0]?.href).toContain('/ux/jobs/j1/steps/s1?');
    expect(result.whom).toBe('IT · human-required jobs');
  });
  // A reserved release the door would grant its actor rides the view as
  // the car's review step marked with its act (design 58c72d95 D4), and
  // opens the yard's held-car row, where the one Release control lives
  // (D5). Every other line still opens its step page.
  it('a reserved release parses, counts, and links the held-car row; a plain line links its step page', () => {
    const release = step({ id: 'rv', kind: 'release', title: 'Release at 9e1576b8, reserved for emp-david', status: 'active', act: 'release', reserved_for: 'emp-david' });
    const parsed = page([job({ id: 'car', kind: 'ship-a-change', steps: [release] })]);
    expect(parsed.rows[0]?.steps[0]).toEqual({ id: 'rv', kind: 'release', title: 'Release at 9e1576b8, reserved for emp-david', status: 'active', human_required: true, act: 'release', reserved_for: 'emp-david' });
    const result = board([job({ id: 'car', kind: 'ship-a-change', steps: [release] }), job({ id: 'j2' })]);
    if (result.kind !== 'rows') throw new Error(result.kind);
    expect(result.count).toEqual({ kind: 'value', text: '2', troubled: false });
    expect(result.lines.map((line) => [line.id, line.href])).toEqual([
      ['rv', HELD_CARS_HREF],
      ['s1', `/ux/jobs/j2/steps/s1?${new URLSearchParams({ from: '/it', from_label: 'Department Map' })}`],
    ]);
    // The lane lives only in the dock's panel, which `?at=dock` opens.
    expect(HELD_CARS_HREF).toBe('/it?at=dock#held-cars');
    expect(() => page([job({ steps: [step({ act: 7 })] })])).toThrow(/missing text/);
  });
  it('one href rule for both boards: release to the held-car row, else the step page', () => {
    expect(needHref({ id: 'c' }, { id: 'rv', act: 'release' }, '?from=x')).toBe('/it?at=dock#held-cars');
    expect(needHref({ id: 'j 1' }, { id: 's/1' })).toBe('/ux/jobs/j%201/steps/s%2F1');
    expect(needHref({ id: 'j' }, { id: 's' }, '?from=x')).toBe('/ux/jobs/j/steps/s?from=x');
  });
  it('does not count closed jobs or pending/completed steps', () => {
    expect(board([job({ status: 'closed' }), job({ id: 'j2', steps: [step({ status: 'pending' }), step({ id: 's2', status: 'completed' })] })]).count).toEqual({ kind: 'zero' });
  });
  it('an old empty API or absent canonical declaration is unread, never zero', () => {
    expect(() => parseNeedsYou({ data: [], total: 0 })).toThrow(/canonical human-required view/);
    expect(() => page([job({ steps: [{ id: 's', kind: 'task', title: 'work', status: 'ready' }] })])).toThrow(/canonical declaration/);
    expect(() => page([], { total: null })).toThrow(/page count/);
    expect(() => page([], { limit: 501 })).toThrow(/page bound/);
  });
  it('shows genuine empty and failed reads in different words', () => {
    expect(board([])).toEqual({ kind: 'empty', count: { kind: 'zero' }, words: 'No open IT jobs require a human.', whom: 'IT · human-required jobs', cut: null });
    expect(needsYouBoard({ kind: 'failed', error: 'HTTP 503' }, NOW)).toEqual({ kind: 'unread', why: 'the read failed — HTTP 503' });
    expect(needsYouBoard({ kind: 'loading' }, NOW)).toEqual({ kind: 'unread', why: 'not read yet' });
  });
  it('conserves all pages before giving an exact distinct-job count', async () => {
    const urls: string[] = [];
    const answer = await fetchNeedsYou(async (url) => {
      urls.push(url);
      return { kind: 'ready', data: page([job({ id: `j${urls.length}` })], { total: 2, limit: 1, offset: urls.length - 1 }) };
    });
    expect(urls).toEqual([`${NEEDS_YOU_URL}&offset=0`, `${NEEDS_YOU_URL}&offset=1`]);
    expect(answered(needsYouBoard(answer, NOW)).count).toEqual({ kind: 'value', text: '2', troubled: false });
  });
  it('refuses duplicate jobs, changing totals, and a missing page rather than inventing a count', async () => {
    for (const fault of ['duplicate', 'changed', 'empty', 'failed']) {
      let calls = 0;
      const answer = await fetchNeedsYou(async () => {
        calls += 1;
        if (calls === 2 && fault === 'failed') return { kind: 'failed', error: 'page 2 HTTP 503' };
        return { kind: 'ready', data: page(calls === 2 && fault === 'empty' ? [] : [job({ id: fault === 'duplicate' ? 'same' : `j${calls}` })], { total: calls === 2 && fault === 'changed' ? 3 : 2, limit: 1, offset: calls - 1 }) };
      });
      expect(answer.kind).toBe('failed');
      expect(calls).toBe(2);
    }
  });
  it('bounds a poll to four thin pages and labels the resulting count a floor', async () => {
    let calls = 0;
    const answer = await fetchNeedsYou(async () => {
      calls += 1;
      return { kind: 'ready', data: page([job({ id: `j${calls}`, steps: calls === 1 ? [step()] : [] })], { total: 5, limit: 1, offset: calls - 1 }) };
    });
    expect(calls).toBe(4);
    const result = answered(needsYouBoard(answer, NOW));
    expect(result.count.kind).toBe('floor');
    if (result.count.kind === 'floor') expect(result.count.text).toBe('1');
  });
  it('a cut answer with no qualifying work never says nothing requires a human', () => {
    const result = board([job({ steps: [] })], { total: 9, truncated: true });
    expect(result.count.kind).toBe('floor');
    if (result.kind === 'empty') expect(result.words).toContain('more may');
  });
});
