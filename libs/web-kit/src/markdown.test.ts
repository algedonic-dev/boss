// The renderer's contract (2244db9e): real markdown renders, hostile
// input stays inert, and the shapes design docs actually use (tables,
// fences, proposals in bold) all work.

import { describe, expect, test } from 'bun:test';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  test('the shapes a design doc uses', () => {
    const html = renderMarkdown(
      '# Title\n\nA **bold** claim with `code` and a [link](https://example.com).\n\n' +
        '- first\n- second\n\n> quoted\n\n```\nlet x = 1 < 2;\n```',
    );
    expect(html).toContain('<h1>Title</h1>');
    expect(html).toContain('<strong>bold</strong>');
    expect(html).toContain('<code>code</code>');
    expect(html).toContain('<a href="https://example.com">link</a>');
    expect(html).toContain('<ul><li>first</li><li>second</li></ul>');
    expect(html).toContain('<blockquote>quoted</blockquote>');
    // Code fences escape their interior.
    expect(html).toContain('let x = 1 &lt; 2;');
  });

  test('pipe tables render — the docs carry them', () => {
    const html = renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |');
    expect(html).toContain('<th>a</th>');
    expect(html).toContain('<td>2</td>');
  });

  test('hostile input stays inert', () => {
    const html = renderMarkdown(
      '<script>alert(1)</script>\n\n[x](javascript:alert(1))\n\n<img src=x onerror=alert(1)>',
    );
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
    // A javascript: href never becomes a link.
    expect(html).not.toContain('href="javascript:');
    expect(html).not.toContain('<img');
  });

  test('relative and https links pass, everything else stays text', () => {
    expect(renderMarkdown('[j](/ux/jobs/abc)')).toContain('href="/ux/jobs/abc"');
    expect(renderMarkdown('[d](data:text/html,x)')).not.toContain('href=');
  });

  test('plain paragraphs join their wrapped lines', () => {
    const html = renderMarkdown('one line\nwrapped onward\n\nsecond para');
    expect(html).toContain('<p>one line wrapped onward</p>');
    expect(html).toContain('<p>second para</p>');
  });
});
