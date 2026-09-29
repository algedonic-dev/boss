// Promoting one of the owner's passkeys to the operator tier — the /me
// half of design 2cb6256f (David, 2026-09-28; backlog 2a228d0c, car 2).
//
// Only an operator-tier passkey raises the owner's session to operator,
// and every enrolled key is user tier, so until one key is promoted the
// "Operator access" button can only refuse. The ceremony is three calls,
// nothing typed:
//
//   1. requestPromotion — "Make this my operator key": the gateway files
//      one passkey-promotion packet for that key (or answers the one
//      already open) and says whether it is approved. Filing grants
//      nothing.
//   2. The owner approves the packet on its own page with his passkey.
//   3. beginPromotion → two touches → finishPromotion: the key being
//      promoted (possession), then a break-glass hardware key (the vouch
//      a copied cookie cannot reach). The gateway checks everything again
//      and only then asks for the flip.
//
// Each touch is its own click on the page: a second WebAuthn prompt with
// no user gesture of its own is refused by Safari.

import { assertionFailure } from './passkeyHints';

const b64urlToBytes = (s: string): Uint8Array => {
  const pad = s.length % 4 === 2 ? '==' : s.length % 4 === 3 ? '=' : '';
  const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
};

const bytesToB64url = (buf: ArrayBuffer): string =>
  btoa(String.fromCharCode(...new Uint8Array(buf)))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');

/** The packet a request filed or found, and whether it is approved. */
export type PromotionRequest = Readonly<{
  packet_id: string;
  label: string;
  approved: boolean;
}>;

/** WebAuthn request options as the gateway sends them (base64url ids). */
export type RequestOptionsJson = Readonly<{
  publicKey: Readonly<{
    challenge: string;
    allowCredentials?: readonly Readonly<{ type: 'public-key'; id: string }>[];
    [key: string]: unknown;
  }>;
}>;

/** A begun ceremony: the two touches it waits on. */
export type PromotionCeremony = Readonly<{
  ceremony_id: string;
  label: string;
  key: RequestOptionsJson;
  vouch: RequestOptionsJson;
}>;

/** An assertion, serialised the way the gateway's verifier reads it. */
export type AssertionJson = Readonly<Record<string, unknown>>;

/** What a finished promotion reports. */
export type Promoted = Readonly<{
  packet_id: string;
  credential_id: string;
  access_tier: string;
  vouched_by: string;
  promoted: boolean;
}>;

async function postJson<T>(path: string, body: unknown, what: string): Promise<T> {
  const resp = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!resp.ok) {
    // The gateway refuses in words; they are thrown as it said them.
    const text = await resp.text().catch(() => '');
    throw new Error(text || `${what} refused (${resp.status})`);
  }
  return (await resp.json()) as T;
}

/** Step 1: file (or find) the promotion packet for one of your keys. */
export function requestPromotion(credentialId: string): Promise<PromotionRequest> {
  return postJson(
    '/api/auth/passkey/promotion/request',
    { credential_id: credentialId },
    'the promotion request',
  );
}

/** Step 3a: the gateway re-judges the approved packet and mints both challenges. */
export function beginPromotion(packetId: string): Promise<PromotionCeremony> {
  return postJson('/api/auth/passkey/promotion/begin', { packet_id: packetId }, 'finishing');
}

/** One touch: the browser's authenticator signs `options`' challenge. */
export async function touch(options: RequestOptionsJson): Promise<AssertionJson> {
  const pk = options.publicKey;
  const allow = pk.allowCredentials ?? [];
  let credential: PublicKeyCredential | null = null;
  try {
    credential = (await navigator.credentials.get({
      publicKey: {
        ...(pk as unknown as PublicKeyCredentialRequestOptions),
        challenge: b64urlToBytes(pk.challenge).buffer as ArrayBuffer,
        allowCredentials: allow.map((c) => ({
          type: c.type,
          id: b64urlToBytes(c.id).buffer as ArrayBuffer,
        })),
      },
    })) as PublicKeyCredential | null;
  } catch (err) {
    throw new Error(assertionFailure(err, allow.length));
  }
  if (!credential) throw new Error('The key returned no credential.');
  const assertion = credential.response as AuthenticatorAssertionResponse;
  return {
    id: credential.id,
    rawId: bytesToB64url(credential.rawId),
    type: credential.type,
    extensions: credential.getClientExtensionResults(),
    response: {
      authenticatorData: bytesToB64url(assertion.authenticatorData),
      clientDataJSON: bytesToB64url(assertion.clientDataJSON),
      signature: bytesToB64url(assertion.signature),
      userHandle: assertion.userHandle ? bytesToB64url(assertion.userHandle) : null,
    },
  };
}

/** Step 3b: both assertions to the gateway, which promotes and closes the packet. */
export function finishPromotion(
  ceremonyId: string,
  key: AssertionJson,
  vouch: AssertionJson,
): Promise<Promoted> {
  return postJson(
    '/api/auth/passkey/promotion/finish',
    { ceremony_id: ceremonyId, key, vouch },
    'the promotion',
  );
}
