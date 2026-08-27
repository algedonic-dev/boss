// Minimal escape-first markdown → HTML (2244db9e).
//
// Every doc/context pane rendered markdown as whitespace-preserved
// TEXT because the SPA had no renderer — the review plugin's fallback,
// the decision-context panel, sign-off v2's case block all carried a
// "swap the interior when one lands" comment. David, 2026-08-19:
// "Showed special markdown characters / expected rendered as normal
// markdown." This is the one that lands.
//
// DELIBERATELY MINIMAL, and escape-first is the contract: packets
// carry operator- and agent-written text, so every character is
// HTML-escaped BEFORE any markdown transform, and link hrefs admit
// only http(s) and site-relative paths — a renderer that passed raw
// HTML through would turn every packet into an injection surface.
// Covered: headings, paragraphs, bold/italic, inline code, fenced
// code blocks, unordered/ordered lists, blockquotes, links, pipe
// tables, horizontal rules. Not covered on purpose: raw HTML, images
// (nothing in a packet should hotlink), nested lists beyond one
// level, footnotes.
//
// Plugins (framework-free bundles) reach this through
// `window.__boss_markdown`, installed once by the SPA at boot — one
// definition, no per-bundle copy to drift (§9a).

function escapeHtml(s: string): string {
  return s
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

function safeHref(raw: string): string | null {
  const href = raw.trim();
  if (/^https?:\/\//i.test(href)) return href;
  if (href.startsWith('/') || href.startsWith('./') || href.startsWith('../')) return href;
  return null;
}

/// Inline transforms over already-escaped text: code spans first (their
/// interior takes no further transforms), then links, bold, italic.
function inline(escaped: string): string {
  const parts = escaped.split(/(`[^`]+`)/g);
  return parts
    .map((part) => {
      if (part.startsWith('`') && part.endsWith('`') && part.length > 2) {
        return `<code>${part.slice(1, -1)}</code>`;
      }
      let out = part.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (m, text, href) => {
        const safe = safeHref(href);
        return safe ? `<a href="${safe}">${text}</a>` : m;
      });
      out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
      out = out.replace(/(^|\W)\*([^*\s][^*]*)\*/g, '$1<em>$2</em>');
      return out;
    })
    .join('');
}

/// Render markdown to HTML. Input is untrusted; output is safe to
/// inject — everything is escaped before any tag this function emits.
export function renderMarkdown(md: string): string {
  const lines = md.replaceAll('\r\n', '\n').split('\n');
  const out: string[] = [];
  let i = 0;
  let para: string[] = [];

  const flushPara = () => {
    if (para.length) {
      out.push(`<p>${inline(escapeHtml(para.join(' ')))}</p>`);
      para = [];
    }
  };

  while (i < lines.length) {
    const line = lines[i]!;

    if (line.startsWith('```')) {
      flushPara();
      const buf: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i]!.startsWith('```')) {
        buf.push(lines[i]!);
        i += 1;
      }
      i += 1; // closing fence (or EOF)
      out.push(`<pre><code>${escapeHtml(buf.join('\n'))}</code></pre>`);
      continue;
    }

    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      flushPara();
      const level = heading[1]!.length;
      out.push(`<h${level}>${inline(escapeHtml(heading[2]!))}</h${level}>`);
      i += 1;
      continue;
    }

    if (/^\s*(---+|\*\*\*+)\s*$/.test(line)) {
      flushPara();
      out.push('<hr>');
      i += 1;
      continue;
    }

    if (/^\s*([-*]|\d+\.)\s+/.test(line)) {
      flushPara();
      const ordered = /^\s*\d+\.\s+/.test(line);
      const items: string[] = [];
      while (i < lines.length && /^\s*([-*]|\d+\.)\s+/.test(lines[i]!)) {
        items.push(lines[i]!.replace(/^\s*([-*]|\d+\.)\s+/, ''));
        // Continuation lines (indented, non-list) fold into the item.
        while (
          i + 1 < lines.length &&
          /^\s{2,}\S/.test(lines[i + 1]!) &&
          !/^\s*([-*]|\d+\.)\s+/.test(lines[i + 1]!)
        ) {
          items[items.length - 1] += ' ' + lines[i + 1]!.trim();
          i += 1;
        }
        i += 1;
      }
      const tag = ordered ? 'ol' : 'ul';
      out.push(
        `<${tag}>${items.map((it) => `<li>${inline(escapeHtml(it))}</li>`).join('')}</${tag}>`,
      );
      continue;
    }

    if (line.startsWith('>')) {
      flushPara();
      const buf: string[] = [];
      while (i < lines.length && lines[i]!.startsWith('>')) {
        buf.push(lines[i]!.replace(/^>\s?/, ''));
        i += 1;
      }
      out.push(`<blockquote>${inline(escapeHtml(buf.join(' ')))}</blockquote>`);
      continue;
    }

    if (line.includes('|') && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|[\s:|-]*$/.test(lines[i + 1]!)) {
      flushPara();
      const cells = (l: string) =>
        l
          .replace(/^\s*\|/, '')
          .replace(/\|\s*$/, '')
          .split('|')
          .map((c) => inline(escapeHtml(c.trim())));
      const head = cells(line);
      i += 2; // header + separator
      const rows: string[][] = [];
      while (i < lines.length && lines[i]!.includes('|')) {
        rows.push(cells(lines[i]!));
        i += 1;
      }
      out.push(
        `<table><thead><tr>${head.map((c) => `<th>${c}</th>`).join('')}</tr></thead>` +
          `<tbody>${rows
            .map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join('')}</tr>`)
            .join('')}</tbody></table>`,
      );
      continue;
    }

    if (line.trim() === '') {
      flushPara();
      i += 1;
      continue;
    }

    para.push(line.trim());
    i += 1;
  }
  flushPara();
  return out.join('\n');
}

/// Install the renderer where framework-free plugin bundles can reach
/// it. Called once from the SPA's bootstrap; bundles feature-test
/// `window.__boss_markdown` and fall back to preserved text when
/// absent (older SPA, tests).
export function installMarkdownForPlugins(): void {
  (window as unknown as Record<string, unknown>).__boss_markdown = renderMarkdown;
}
