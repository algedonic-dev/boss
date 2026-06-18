// Step-plugin host — runtime glue that lets plugin bundles register
// their mount function and the Svelte dispatcher invoke it on demand.
//
// Plugins ship as static .js files under `/plugins/<frontend_url>`
// (served by the gateway from /var/lib/boss/step-plugins/). When
// the script executes, it calls
//   window.__boss_register_step_plugin(kind, mount)
// where `mount` is a plain-DOM function. The dispatcher creates a
// container `<div>` and calls mount(container, props); the plugin
// renders into the container and returns an optional cleanup fn.
//
// Framework-agnostic: plugins can use vanilla DOM, lit-html, or
// bundle their own micro-library. The host has zero runtime
// dependency on React or any other framework.

import type { StepPluginSpec } from '../it/step-plugins/stepPluginTypes';

export type PluginCurrentUser = {
  id: string;
  role: string;
};

export type StepPluginProps = {
  step: {
    id: string;
    kind: string;
    title: string;
    status: string;
    assignee_id: string | null;
    sort_order: number;
    sign_offs_required?: string[];
    sign_offs?: {
      authority_id: string;
      role: string;
      stamped_at: string;
      shape_hash: string;
    }[];
    metadata: Record<string, unknown>;
    notes: string | null;
  };
  jobId: string;
  onUpdate: () => void;
  currentUser?: PluginCurrentUser;
};

export type PluginCleanup = () => void;

export type StepPluginMountFn = (
  container: HTMLElement,
  props: StepPluginProps,
) => PluginCleanup | void;

type Registry = Map<string, StepPluginMountFn>;
type PendingResolver = (m: StepPluginMountFn) => void;

const registry: Registry = new Map();
const pending: Map<string, PendingResolver[]> = new Map();
const inflight: Map<string, Promise<StepPluginMountFn | null>> = new Map();

// Map of step-kind → spec for kinds with an active plugin, primed
// from `GET /api/jobs/step-plugins` so we don't hit the per-kind
// endpoint (and log a 404) for the common case of "no plugin
// registered, render the default surface."
let activeSpecsPromise: Promise<Map<string, StepPluginSpec>> | null = null;

function loadActiveSpecs(): Promise<Map<string, StepPluginSpec>> {
  if (!activeSpecsPromise) {
    activeSpecsPromise = (async () => {
      const resp = await fetch('/api/jobs/step-plugins');
      if (!resp.ok) return new Map<string, StepPluginSpec>();
      const list = (await resp.json()) as StepPluginSpec[];
      return new Map(list.map((s) => [s.kind, s]));
    })().catch(() => new Map<string, StepPluginSpec>());
  }
  return activeSpecsPromise;
}

/// True iff the boss-jobs step-plugin registry has an active row
/// for `kind`. The dispatcher uses this to decide between mounting
/// a real plugin (tier 2) and falling back to GenericSurface
/// (tier 3). Cached after the first call so re-checking on every
/// step render is free.
export async function hasActivePluginFor(kind: string): Promise<boolean> {
  const specs = await loadActiveSpecs();
  return specs.has(kind);
}

export function installStepPluginHost(): void {
  (window as unknown as {
    __boss_register_step_plugin?: (kind: string, mount: StepPluginMountFn) => void;
  }).__boss_register_step_plugin = (kind, mount) => {
    registry.set(kind, mount);
    const waiters = pending.get(kind) ?? [];
    pending.delete(kind);
    for (const w of waiters) w(mount);
  };
}

export async function getStepPluginMount(
  kind: string,
): Promise<StepPluginMountFn | null> {
  const cached = registry.get(kind);
  if (cached) return cached;

  const existing = inflight.get(kind);
  if (existing) return existing;

  const load = loadPlugin(kind).finally(() => inflight.delete(kind));
  inflight.set(kind, load);
  return load;
}

async function loadPlugin(kind: string): Promise<StepPluginMountFn | null> {
  const specs = await loadActiveSpecs();
  const spec = specs.get(kind);
  if (!spec) return null;
  const url = `/plugins/${spec.frontend_url.replace(/^\//, '')}`;

  return new Promise<StepPluginMountFn | null>((resolve) => {
    const prev = pending.get(kind) ?? [];
    prev.push(resolve);
    pending.set(kind, prev);

    const script = document.createElement('script');
    script.src = url;
    script.async = true;
    script.onerror = () => {
      const bucket = pending.get(kind);
      if (bucket) {
        const idx = bucket.indexOf(resolve);
        if (idx >= 0) bucket.splice(idx, 1);
      }
      resolve(null);
    };
    document.head.appendChild(script);
  });
}

export function _resetPluginRegistryForTests(): void {
  registry.clear();
  pending.clear();
  inflight.clear();
  activeSpecsPromise = null;
}
