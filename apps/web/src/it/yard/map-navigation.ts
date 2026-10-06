/** Presentation scope only: the existing station/section identity and
 * its authorized inspection doors stay on the same /it route. */
export function presentationHref(href: string, overview: boolean): string {
  return overview ? `${href}${href.includes('?') ? '&' : '?'}view=overview` : href;
}
