<script lang="ts">
  // The files attached to a step, each with a link that downloads it
  // (user-feedback 2b7116ca, 2026-09-30). The recovery-sheet reprint
  // verb attaches its PDF to the print step (target_kind=step) and
  // reads it back hash-checked, and the packet tells the operator to
  // download it from that step — but no surface listed a step's
  // attachments, so David stood on the step with nothing to click.
  //
  // Step chrome, not step UX: the full-page route draws it for every
  // kind, beside whatever the plugin or the generic surface draws, so
  // no bundle has to learn about files to have them shown.
  import {
    downloadHref,
    formatBytes,
    listFilesFor,
    type FileRef,
  } from '../content/files';
  import { safeLinkHref } from '@boss/web-kit/links';

  let { stepId } = $props<{ stepId: string }>();

  // `unconfigured` (the deployment has no [files] block) draws nothing:
  // no file can be attached there, so there is nothing to miss. An
  // empty list draws nothing too — most steps carry no file. A FAILED
  // read is the one state that must speak, because unread and empty
  // otherwise look the same.
  type Files =
    | { kind: 'loading' }
    | { kind: 'ok'; files: ReadonlyArray<FileRef> }
    | { kind: 'failed'; error: string };

  let files = $state<Files>({ kind: 'loading' });

  $effect(() => {
    const id = stepId;
    let cancelled = false;
    files = { kind: 'loading' };
    listFilesFor('step', id).then(
      (r) => {
        if (cancelled) return;
        files = {
          kind: 'ok',
          files: r.kind === 'ok' ? r.files.filter((f) => f.deleted_at == null) : [],
        };
      },
      (e: unknown) => {
        if (cancelled) return;
        files = { kind: 'failed', error: e instanceof Error ? e.message : String(e) };
      },
    );
    return () => {
      cancelled = true;
    };
  });
</script>

{#if files.kind === 'failed'}
  <p class="step-files-failed load-failed" role="alert" data-testid="step-files-failed">
    Could not read the files attached to this step ({files.error}). A file may be
    attached that this page cannot show.
  </p>
{:else if files.kind === 'ok' && files.files.length > 0}
  <section class="step-files" data-testid="step-files" aria-label="Files attached to this step">
    <h2>Files attached to this step</h2>
    <ul>
      {#each files.files as f (f.id)}
        <li>
          <a href={safeLinkHref(downloadHref(f.id))} download={f.filename}>{f.filename}</a>
          <span class="step-files-size">{formatBytes(f.size_bytes)}</span>
          {#if f.sha256}
            <code class="step-files-sha" title="sha256">sha256 {f.sha256}</code>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .step-files,
  .step-files-failed {
    /* A flex item of the page's column: never squeezed by the body. */
    flex: none;
    margin: 10px 24px 0;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 10px 14px;
    background: var(--card);
  }
  .step-files h2 {
    margin: 0;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--static);
    font-weight: 600;
  }
  .step-files ul {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
  }
  .step-files li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 12px;
    font-size: 13.5px;
    padding: 2px 0;
  }
  .step-files a {
    font-weight: 600;
  }
  .step-files-size {
    color: var(--text-dim);
    font-size: 12px;
  }
  .step-files-sha {
    color: var(--text-dim);
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .step-files-failed {
    font-size: 13px;
  }
</style>
