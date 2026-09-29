<script lang="ts">
  // Passkey enrolment — the identity half of the presence ceremony
  // (docs/design/presence.md; packet 7218c3f1). Enrolment happens
  // behind the already-authenticated session: the gateway mints the
  // creation challenge, the browser's authenticator answers, and the
  // credential lands in BOSS's own table. Presence-gated steps then
  // verify fresh assertions against these credentials, bound to each
  // step's shape hash.
  import { PHONE_QR_HINT } from './passkeyHints';
  import { elevateSession, enrollPasskey } from '../steps/presence';
  import {
    beginPromotion,
    finishPromotion,
    requestPromotion,
    touch,
    type AssertionJson,
    type PromotionCeremony,
  } from './promotion';
  import { formatDate } from '@boss/web-kit/ui/date';

  type CredentialRow = Readonly<{
    credential_id: string;
    label: string;
    registered_at: string;
    last_used_at: string | null;
    /** `user` (every enrolment) or `operator` (promoted, design 2cb6256f). */
    access_tier?: string;
  }>;

  type PanelState =
    | { kind: 'loading' }
    | { kind: 'unavailable' }
    | { kind: 'ready'; credentials: readonly CredentialRow[] };

  let panel = $state<PanelState>({ kind: 'loading' });
  let label = $state('');
  let busy = $state(false);
  let error = $state('');

  async function refresh(): Promise<void> {
    const resp = await fetch('/api/auth/passkey/credentials').catch(() => null);
    if (!resp || !resp.ok) {
      // 401 (no session), 403 (guest), or the ceremony not mounted:
      // this panel simply isn't for this viewer.
      panel = { kind: 'unavailable' };
      return;
    }
    const credentials = (await resp.json()) as CredentialRow[];
    panel = { kind: 'ready', credentials };
  }

  async function add(): Promise<void> {
    busy = true;
    error = '';
    try {
      await enrollPasskey(label.trim() || 'passkey');
      label = '';
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // Removal: a lost or retired authenticator can go, the LAST one
  // stays. David, feedback 16414d99: "Important so users can add a
  // backup key, but don't let them delete all their keys." The server
  // enforces the rule (409, with the way out in its text); the button
  // says the same thing before the click, so a single key never
  // offers a control that can only refuse.
  const LAST_KEY_STAYS = 'Your last passkey stays — add a backup key first, then remove this one.';

  async function remove(cred: CredentialRow): Promise<void> {
    if (!confirm(`Remove the passkey "${cred.label}"? It can no longer sign approvals.`)) return;
    busy = true;
    error = '';
    try {
      const resp = await fetch(
        `/api/auth/passkey/credentials/${encodeURIComponent(cred.credential_id)}`,
        { method: 'DELETE' },
      );
      if (!resp.ok) {
        const text = await resp.text();
        error = text || `Could not remove the passkey (${resp.status}).`;
      }
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // Operator access (backlog 3c92c5b8): the platform owner's passkey
  // raises this session to the operator tier, which the IT registries
  // (Agents, Credentials) require. The gateway decides who the owner is
  // and refuses anyone else in its own words; the tier ends with the
  // session.
  let elevatedUntil = $state('');

  async function elevate(): Promise<void> {
    busy = true;
    error = '';
    try {
      const granted = await elevateSession();
      elevatedUntil = formatDate(new Date(granted.expires_at * 1000).toISOString());
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // Operator key (design 2cb6256f): only an operator-tier key opens
  // "Operator access", and a key becomes one by a recorded act — a
  // packet you approve with your passkey, then two touches here: the
  // key itself, then a break-glass hardware key. Each touch is its own
  // click, because a second passkey prompt without one is refused by
  // Safari.
  type Promotion =
    | { kind: 'idle' }
    | { kind: 'awaiting-approval'; label: string; packetId: string }
    | { kind: 'touch-key'; ceremony: PromotionCeremony }
    | { kind: 'touch-vouch'; ceremony: PromotionCeremony; key: AssertionJson }
    | { kind: 'done'; label: string };

  let promotion = $state<Promotion>({ kind: 'idle' });

  async function step(run: () => Promise<void>): Promise<void> {
    busy = true;
    error = '';
    try {
      await run();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      promotion = { kind: 'idle' };
    } finally {
      busy = false;
    }
  }

  const makeOperator = (cred: CredentialRow) =>
    step(async () => {
      const req = await requestPromotion(cred.credential_id);
      promotion = req.approved
        ? { kind: 'touch-key', ceremony: await beginPromotion(req.packet_id) }
        : { kind: 'awaiting-approval', label: req.label, packetId: req.packet_id };
    });

  const touchKey = (ceremony: PromotionCeremony) =>
    step(async () => {
      promotion = { kind: 'touch-vouch', ceremony, key: await touch(ceremony.key) };
    });

  const touchVouch = (ceremony: PromotionCeremony, key: AssertionJson) =>
    step(async () => {
      await finishPromotion(ceremony.ceremony_id, key, await touch(ceremony.vouch));
      promotion = { kind: 'done', label: ceremony.label };
      await refresh();
    });

  $effect(() => {
    void refresh();
  });
</script>

{#if panel.kind === 'ready'}
  <div class="passkeys-panel">
    <h3>Passkeys</h3>
    <p class="passkeys-blurb">
      Steps that demand proof of presence ask your passkey to sign the
      exact content being approved. Enrol one here; approvals then
      prompt for it in place. Keep a backup key enrolled — a lost one
      can be removed, but the last one always stays.
    </p>
    {#if panel.credentials.length === 0}
      <p class="passkeys-empty">No passkey enrolled yet.</p>
    {:else}
      <ul class="passkeys-list">
        {#each panel.credentials as cred (cred.credential_id)}
          <li>
            <span class="passkeys-label">{cred.label}</span>
            <span class="passkeys-meta">
              enrolled {formatDate(cred.registered_at)}
              {#if cred.last_used_at}
                · last used {formatDate(cred.last_used_at)}
              {:else}
                · never used
              {/if}
              {#if cred.access_tier === 'operator'}
                · <strong>operator key</strong>
              {/if}
            </span>
            {#if cred.access_tier === 'user'}
              <button
                class="passkeys-promote"
                onclick={() => void makeOperator(cred)}
                disabled={busy}
                title="Platform owner only: a packet you approve with your passkey, then a touch of this key and of a break-glass key"
              >
                Make this my operator key
              </button>
            {/if}
            <button
              class="passkeys-remove"
              onclick={() => void remove(cred)}
              disabled={busy || panel.credentials.length === 1}
              title={panel.credentials.length === 1 ? LAST_KEY_STAYS : `Remove ${cred.label}`}
            >
              Remove
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="passkeys-add">
      <input
        type="text"
        placeholder="label (e.g. yubikey)"
        bind:value={label}
        disabled={busy}
      />
      <button onclick={() => void add()} disabled={busy}>
        {busy ? 'Waiting for authenticator…' : 'Add passkey'}
      </button>
    </div>
    <!-- The browser draws the enrolment dialog and its phone QR; this
         page owes the sentence the dialog does not say (a55d9a01). -->
    <p class="passkeys-hint">{PHONE_QR_HINT}</p>
    {#if promotion.kind === 'awaiting-approval'}
      <p class="passkeys-promotion">
        Requested: <a href={`/jobs/${promotion.packetId}`}>approve making {promotion.label} an
        operator key</a> with your passkey, then press "Make this my operator key" again to finish.
      </p>
    {:else if promotion.kind === 'touch-key'}
      {@const ceremony = promotion.ceremony}
      <div class="passkeys-promotion">
        <button onclick={() => void touchKey(ceremony)} disabled={busy}>
          Touch {ceremony.label}
        </button>
        <span class="passkeys-meta">1 of 2 · the key being promoted</span>
      </div>
    {:else if promotion.kind === 'touch-vouch'}
      {@const ceremony = promotion.ceremony}
      {@const key = promotion.key}
      <div class="passkeys-promotion">
        <button onclick={() => void touchVouch(ceremony, key)} disabled={busy}>
          Touch your break-glass key
        </button>
        <span class="passkeys-meta">2 of 2 · primary or backup</span>
      </div>
    {:else if promotion.kind === 'done'}
      <p class="passkeys-promotion">
        {promotion.label} is now your operator key — "Operator access with your passkey" uses it.
      </p>
    {/if}
    {#if panel.credentials.length > 0}
      <div class="passkeys-elevate">
        <button onclick={() => void elevate()} disabled={busy}>
          Operator access with your passkey
        </button>
        {#if elevatedUntil}
          <span class="passkeys-meta">operator until {elevatedUntil}</span>
        {:else}
          <span class="passkeys-meta">platform owner only · ends with this session</span>
        {/if}
      </div>
    {/if}
    {#if error}
      <p class="passkeys-error">{error}</p>
    {/if}
  </div>
{/if}

<style>
  .passkeys-panel {
    margin-top: 1rem;
  }
  .passkeys-hint {
    margin: 0.5rem 0 0;
    font-size: 0.85rem;
    color: var(--static);
    max-width: 60ch;
  }
  .passkeys-panel h3 {
    margin: 0 0 0.25rem;
    font-size: 0.95rem;
  }
  .passkeys-blurb,
  .passkeys-empty {
    margin: 0 0 0.5rem;
    font-size: 0.85rem;
    color: var(--static);
  }
  .passkeys-list {
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
  }
  .passkeys-list li {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    padding: 0.15rem 0;
  }
  .passkeys-label {
    font-weight: 600;
  }
  .passkeys-meta {
    font-size: 0.8rem;
    color: var(--static);
  }
  .passkeys-remove {
    margin-left: auto;
    font-size: 0.75rem;
  }
  .passkeys-remove:disabled {
    cursor: not-allowed;
  }
  .passkeys-promote {
    font-size: 0.75rem;
  }
  .passkeys-promotion {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    margin: 0.5rem 0 0;
    font-size: 0.85rem;
  }
  .passkeys-add {
    display: flex;
    gap: 0.5rem;
  }
  .passkeys-elevate {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    margin-top: 0.5rem;
  }
  .passkeys-error {
    margin: 0.5rem 0 0;
    font-size: 0.85rem;
    color: var(--err);
  }
</style>
