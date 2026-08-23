<script lang="ts">
  // Generic status chip — the closed-tone counterpart to the app's
  // zoo of hand-rolled `chip chip-<domain>-<state>` spans. The tone
  // set is deliberately closed (registry-style: five semantic tones,
  // not one class per domain state) and the classes are static
  // strings so grep finds every user of a tone.
  //
  // The five .chip-tone-* rules live in apps/web/src/styles.css
  // alongside the base .chip rule.
  type Tone = 'ok' | 'warn' | 'err' | 'muted' | 'active';

  let { value, tone } = $props<{
    /// Raw status value; kebab/snake-case is humanized for display
    /// ("in-repair" → "in repair") — chips render lowercase by
    /// convention, so no capitalization.
    value: string;
    tone: Tone;
  }>();

  let label = $derived(value.replace(/[-_]/g, ' '));
</script>

{#if tone === 'ok'}
  <span class="chip chip-tone-ok">{label}</span>
{:else if tone === 'warn'}
  <span class="chip chip-tone-warn">{label}</span>
{:else if tone === 'err'}
  <span class="chip chip-tone-err">{label}</span>
{:else if tone === 'active'}
  <span class="chip chip-tone-active">{label}</span>
{:else}
  <span class="chip chip-tone-muted">{label}</span>
{/if}
