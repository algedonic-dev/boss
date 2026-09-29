import { afterEach, describe, expect, test } from 'bun:test';
import { beginPromotion, finishPromotion, requestPromotion, touch } from './promotion';

// Design 2cb6256f, car 2: the /me half of promoting a passkey to the
// operator tier. These pin what the page sends each gateway door and
// that a refusal reaches the owner in the gateway's own words.

const realFetch = globalThis.fetch;
const realNavigator = globalThis.navigator;

afterEach(() => {
  globalThis.fetch = realFetch;
  Object.defineProperty(globalThis, 'navigator', { value: realNavigator, configurable: true });
});

type Call = { url: string; body: unknown };

function recordFetch(reply: (url: string) => Response): Call[] {
  const calls: Call[] = [];
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : null });
    return reply(url);
  }) as typeof fetch;
  return calls;
}

describe('the promotion ceremony talks to the gateway doors', () => {
  test('the request names the key, and filing grants nothing', async () => {
    const calls = recordFetch(
      () => new Response(JSON.stringify({ packet_id: 'p1', label: 'yubikey', approved: false })),
    );
    const out = await requestPromotion('Y3JlZA');
    expect(calls).toEqual([
      { url: '/api/auth/passkey/promotion/request', body: { credential_id: 'Y3JlZA' } },
    ]);
    expect(out.approved).toBe(false);
  });

  test('a refusal is thrown in the gateway’s own words', async () => {
    recordFetch(
      () => new Response('yubikey is already an operator key — there is nothing to promote', {
        status: 409,
      }),
    );
    await expect(requestPromotion('Y3JlZA')).rejects.toThrow('already an operator key');
  });

  test('begin names the packet; finish carries both assertions', async () => {
    const calls = recordFetch(() => new Response(JSON.stringify({ ok: true })));
    await beginPromotion('p1');
    await finishPromotion('c1', { id: 'k' }, { id: 'v' });
    expect(calls).toEqual([
      { url: '/api/auth/passkey/promotion/begin', body: { packet_id: 'p1' } },
      {
        url: '/api/auth/passkey/promotion/finish',
        body: { ceremony_id: 'c1', key: { id: 'k' }, vouch: { id: 'v' } },
      },
    ]);
  });
});

describe('a touch signs exactly the challenge it was given', () => {
  test('the challenge and allowed ids are decoded, the assertion encoded back', async () => {
    let asked: CredentialRequestOptions | undefined;
    const bytes = (s: string) => new TextEncoder().encode(s).buffer as ArrayBuffer;
    Object.defineProperty(globalThis, 'navigator', {
      configurable: true,
      value: {
        credentials: {
          get: async (opts: CredentialRequestOptions) => {
            asked = opts;
            return {
              id: 'cred',
              rawId: bytes('cred'),
              type: 'public-key',
              getClientExtensionResults: () => ({}),
              response: {
                authenticatorData: bytes('ad'),
                clientDataJSON: bytes('cd'),
                signature: bytes('sig'),
                userHandle: null,
              },
            };
          },
        },
      },
    });
    const out = await touch({
      publicKey: {
        challenge: 'AQID',
        allowCredentials: [{ type: 'public-key', id: 'BAUG' }],
        userVerification: 'required',
      },
    });
    const pk = asked?.publicKey;
    expect(Array.from(new Uint8Array(pk?.challenge as ArrayBuffer))).toEqual([1, 2, 3]);
    const id = pk?.allowCredentials?.[0]?.id as ArrayBuffer;
    expect(Array.from(new Uint8Array(id))).toEqual([4, 5, 6]);
    expect(pk?.userVerification).toBe('required');
    expect(out).toEqual({
      id: 'cred',
      rawId: 'Y3JlZA',
      type: 'public-key',
      extensions: {},
      response: {
        authenticatorData: 'YWQ',
        clientDataJSON: 'Y2Q',
        signature: 'c2ln',
        userHandle: null,
      },
    });
  });
});
