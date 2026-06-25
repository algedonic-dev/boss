// Shared client-side navigation. Each app owns its own Route union +
// parseRoute; only these origin-relative helpers are shared.
export function navigate(path: string): void {
  window.history.pushState({}, '', path);
  window.dispatchEvent(new PopStateEvent('popstate'));
}
/** href factory for a mount prefix, e.g. makeHref('/simulator'). */
export function makeHref(basePrefix: string): (relative: string) => string {
  return (relative: string): string =>
    basePrefix + (relative.startsWith('/') ? relative : `/${relative}`);
}
/** Default href — auto-detects the /dashboard mount (apps/web behavior). */
export function href(relative: string): string {
  const base = window.location.pathname.startsWith('/dashboard') ? '/dashboard' : '';
  return base + (relative.startsWith('/') ? relative : `/${relative}`);
}
