import { renderMarkdown } from '@boss/web-kit/markdown';
import { entityHref, ID_IS_LABEL } from '@boss/web-kit/ui/entity-href';
import { tokenize } from './richBody';

const escapeText = (text: string): string => text.replaceAll('&', '&amp;')
  .replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');

// Only the escape-first renderer produces markup. Promote IDs in its
// text segments, never in attributes, authored links or literal code.
export function renderRichMarkdown(body: string, employeeNames?: ReadonlyMap<string, string>): string {
  let literalDepth = 0;
  return renderMarkdown(body).split(/(<[^>]+>)/u).map((segment) => {
    if (segment.startsWith('<')) {
      if (/^<(a|code|pre)(?:\s|>)/u.test(segment)) literalDepth += 1;
      if (/^<\/(a|code|pre)>/u.test(segment)) literalDepth -= 1;
      return segment;
    }
    if (literalDepth > 0) return segment;
    return tokenize(segment).map((token) => {
      if (token.kind === 'text') return token.text;
      const label = token.entityKind === 'employee' ? employeeNames?.get(token.id) : undefined;
      const title = label && label !== token.id ? ` title="${escapeText(token.id)}"` : '';
      const classes = ID_IS_LABEL.has(token.entityKind) ? 'entity-link mono' : 'entity-link';
      return `<a href="${escapeText(entityHref(token.entityKind, token.id))}" class="${classes}"><span${title}>${escapeText(label || token.id)}</span></a>`;
    }).join('');
  }).join('');
}
