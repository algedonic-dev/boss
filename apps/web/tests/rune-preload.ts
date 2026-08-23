// Bun unit-test preload: minimal Svelte-rune shims.
//
// The unit runner executes plain TypeScript — there is no Svelte
// compiler pass — so importing a `*.svelte.ts` module (the session
// envelope, for one) hits a bare `$state(...)` call and throws
// `ReferenceError: $state is not defined` before a single assertion
// runs. Unit tests assert on VALUES and TRANSITIONS, never on
// reactivity (rendering is the mocked suite's layer), so an identity
// function is the whole rune: `$state(v)` hands back `v`, and Svelte's
// deep-proxy behaviour is irrelevant to a value assertion.
//
// Only the runes that module-level code actually evaluates at import
// time are shimmed. If a future import needs `$derived`/`$effect`,
// the ReferenceError will name it — add it here, as identity/no-op.

type RuneShims = {
  $state?: <T>(v: T) => T;
};

(globalThis as unknown as RuneShims).$state = <T>(v: T): T => v;
