// Svelte action for clickable table rows — the click + role +
// tabindex + Enter/Space pattern tables hand-roll on <tr> (see
// finance/TrialBalanceTab for the ancestor), packaged once:
//
//   <tr use:rowLink={{ onActivate: () => navigate(href), label: name }}>
//
// Navigation stays the caller's job (pass a closure over the app's
// `navigate`) so web-kit doesn't grow a router dependency. Rows
// that *navigate* get role="link"; rows that toggle in place should
// keep a hand-rolled role="button" instead.

export type RowLinkParams = Readonly<{
  /// Fires on click, Enter, and Space.
  onActivate: () => void;
  /// Accessible name for the row link; screen readers otherwise
  /// read the entire row content.
  label?: string;
}>;

export function rowLink(
  node: HTMLElement,
  params: RowLinkParams,
): { update(next: RowLinkParams): void; destroy(): void } {
  let current = params;

  node.setAttribute('role', 'link');
  node.tabIndex = 0;
  node.style.cursor = 'pointer';

  function applyLabel(): void {
    if (current.label) {
      node.setAttribute('aria-label', current.label);
    } else {
      node.removeAttribute('aria-label');
    }
  }
  applyLabel();

  function onClick(): void {
    current.onActivate();
  }
  function onKeydown(e: KeyboardEvent): void {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      current.onActivate();
    }
  }
  node.addEventListener('click', onClick);
  node.addEventListener('keydown', onKeydown);

  return {
    update(next: RowLinkParams): void {
      current = next;
      applyLabel();
    },
    destroy(): void {
      node.removeEventListener('click', onClick);
      node.removeEventListener('keydown', onKeydown);
    },
  };
}
