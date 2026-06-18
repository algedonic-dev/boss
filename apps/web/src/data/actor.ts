/**
 * Format an actor id — the audit-log `_actor` / event `actor_id` — into a
 * human-readable label.
 *
 * Every transition is fired by exactly one of two kinds of CPU:
 *   - a **human**, a bare employee id (`emp-032`); resolved to a name when
 *     the caller passes an `empNames` map, otherwise shown as the id.
 *   - a **named automation**, carrying the `automation:` prefix with an
 *     explicit authority — a dispatch rule (`automation:rule:<name>`), the
 *     dispatcher, the simulator, or the emitting service.
 *
 * There is deliberately no anonymous "system" actor (removed in v1.1.0). A
 * legacy null/empty reads as the `platform` automation — never a fake human.
 */
export function formatActor(
  actorId: string | null | undefined,
  empNames?: ReadonlyMap<string, string>,
): string {
  if (!actorId) return 'Platform';

  if (!actorId.startsWith('automation:')) {
    // Human — an employee id. Use a friendly name when we have one.
    return empNames?.get(actorId) ?? actorId;
  }

  const authority = actorId.slice('automation:'.length);
  // A dispatch rule names the rule that fired the side-effect.
  if (authority.startsWith('rule:')) {
    return `Rule · ${authority.slice('rule:'.length)}`;
  }
  const KNOWN: Readonly<Record<string, string>> = {
    dispatcher: 'Dispatcher',
    sim: 'Simulator',
    platform: 'Platform',
  };
  // Otherwise it's a service slug (`automation:account-provisioning`); title-case it.
  return KNOWN[authority] ?? titleCase(authority);
}

function titleCase(slug: string): string {
  return slug
    .split('-')
    .map((w) => (w ? w[0]!.toUpperCase() + w.slice(1) : w))
    .join(' ');
}
