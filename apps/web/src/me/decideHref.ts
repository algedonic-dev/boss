// Where "open the full page" goes from the decide modal.
//
// The step-focus route takes `from`/`from_label` so its Back button
// returns to the lens that sent the operator (feedback 40fe7291). The
// modal is that lens for My Day, so the pair is fixed here — and built
// through URLSearchParams because a label is prose ("My Day") and a
// hand-concatenated query would break on the first space or ampersand.
export function stepFocusHref(jobId: string, stepId: string): string {
  const q = new URLSearchParams({ from: '/ux/me', from_label: 'My Day' });
  return `/ux/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(stepId)}?${q}`;
}
