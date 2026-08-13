/**
 * Format an actor id — the audit-log `_actor` / event `actor_id` — into a
 * human-readable label.
 *
 * Every transition is fired by exactly one of three kinds of CPU (the
 * `ActorId` union in `crates/core/boss-core/src/actor.rs`):
 *   - a **human**, a bare employee id (`emp-032`); resolved to a name when
 *     the caller passes an `empNames` map, otherwise shown as the id.
 *   - a **named automation**, carrying the `automation:` prefix with an
 *     explicit authority — a dispatch rule (`automation:rule:<name>`), the
 *     dispatcher, the simulator, or the emitting service.
 *   - an **agent session**, `<mode>:<model>` (`claude:opus-5`) — an LLM CPU.
 *     `claude` is the mode for an interactive Claude session.
 *
 * The branch order below mirrors `ActorId::from_str` and is load-bearing:
 * `automation:` is claimed first because its slug may itself carry colons
 * (`automation:rule:bill-approve`), then any remaining colon-bearing id is an
 * agent, and only a colon-free id is an employee. Get that order wrong and
 * an agent renders as a missing employee — which is what it used to do.
 *
 * There is deliberately no anonymous "system" actor (removed in v1.1.0). A
 * legacy null/empty reads as the `platform` automation — never a fake human.
 */
export function isHumanActor(actorId: string | null | undefined): boolean {
  // The TS mirror of `ActorId::is_human`. A colon is the whole test: every
  // machine spelling carries one (`automation:*`, the legacy bare `rule:*`,
  // and `<mode>:<model>` agents) and no employee id does. This is the one
  // definition of that rule on the client — callers that need "is this the
  // machine?" negate it rather than re-deriving it.
  return !!actorId && !actorId.includes(':');
}

/** @see the module docstring — this is the label side of {@link isHumanActor}. */
export function formatActor(
  actorId: string | null | undefined,
  empNames?: ReadonlyMap<string, string>,
): string {
  if (!actorId) return 'Platform';

  if (actorId.startsWith('automation:')) {
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

  const split = actorId.indexOf(':');
  if (split > -1) {
    // Agent — `<mode> · <model>`, same separator the rule chip uses. The
    // model half stays verbatim: it is a vendor string, and title-casing
    // `opus-5` into `Opus 5` would name a model that does not exist.
    return `${titleCase(actorId.slice(0, split))} · ${actorId.slice(split + 1)}`;
  }

  // Human — an employee id. Use a friendly name when we have one.
  return empNames?.get(actorId) ?? actorId;
}

function titleCase(slug: string): string {
  return slug
    .split('-')
    .map((w) => (w ? w[0]!.toUpperCase() + w.slice(1) : w))
    .join(' ');
}
