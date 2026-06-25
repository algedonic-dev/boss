// Client-side route model for the Simulator UX. The app is served
// under /simulator in production and at root in dev; parseRoute strips
// a leading /simulator either way so the two environments resolve the
// same routes.
//
// Navigation helpers are shared (web-kit/nav); the Route union +
// parseRoute are app-owned, per the web-kit convention that each app
// declares its own routes.

import { makeHref, navigate } from '@boss/web-kit/nav';

export type Route =
  // The demo cockpit — read-only live window into the brewery. Home.
  | { kind: 'cockpit' }
  // Engine controls — pause/resume, restart epoch, configure.
  | { kind: 'controls' };

/** Parse a pathname into a Route. A leading `/simulator` mount prefix
 *  is stripped first so dev (served at root) and prod (served under
 *  /simulator) resolve identically. Unknown paths fall back to the
 *  cockpit (home). */
export function parseRoute(pathname: string): Route {
  // Strip the mount prefix if present.
  let path = pathname;
  if (path === '/simulator') path = '/';
  else if (path.startsWith('/simulator/')) path = path.slice('/simulator'.length);
  // Normalise a trailing slash (but keep the root "/").
  if (path.length > 1 && path.endsWith('/')) path = path.slice(0, -1);

  if (path === '/controls') return { kind: 'controls' };
  return { kind: 'cockpit' };
}

/** href factory rooted at the /simulator mount. */
export const href = makeHref('/simulator');

export { navigate };
