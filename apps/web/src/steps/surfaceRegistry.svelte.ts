// Surface registry — kind → surface id, fetched from the StepType
// registry (docs/architecture-decisions.md §Step UX & frontend).
// The platform-shipped surface components
// and tenant StepPlugins are the two suppliers of any surface; the
// mapping from a step KIND to its surface is registry DATA, never a
// kind match in code (enforced by infra/lint/no-step-kind-match.sh).
//
// Loaded once per session, same pattern as the tenant manifest.
// While loading (or if the registry is unreachable) every kind maps
// to 'generic', which renders the universal fields/notes surface —
// a degraded-but-usable state, not a blank.

type RegistryState =
  | { kind: 'loading' }
  | { kind: 'ready'; surfaces: Readonly<Record<string, string>> }
  | { kind: 'error' };

export const stepTypeRegistry = $state<{ value: RegistryState }>({
  value: { kind: 'loading' },
});

export async function loadStepTypeRegistry(): Promise<void> {
  try {
    const r = await fetch('/api/jobs/step-types');
    if (!r.ok) {
      stepTypeRegistry.value = { kind: 'error' };
      return;
    }
    const body = (await r.json()) as { kind: string; surface?: string }[];
    const surfaces: Record<string, string> = {};
    for (const t of body) {
      surfaces[t.kind] = t.surface ?? 'generic';
    }
    stepTypeRegistry.value = { kind: 'ready', surfaces };
  } catch {
    stepTypeRegistry.value = { kind: 'error' };
  }
}

/// The surface id a step kind mounts. 'generic' while loading or
/// for kinds the registry doesn't know.
export function surfaceOf(kind: string): string {
  if (stepTypeRegistry.value.kind !== 'ready') return 'generic';
  return stepTypeRegistry.value.surfaces[kind] ?? 'generic';
}
