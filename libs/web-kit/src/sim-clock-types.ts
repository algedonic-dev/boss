// Shared sim-clock wire type. Moved out of apps/web/src/landing/types.ts
// so the sim-clock store + any second app can consume it without
// reaching into apps/web. landing/types re-exports this for existing
// importers.

export type SimClockState = Readonly<{
  /** Full sim-time instant (ISO 8601). Renders date + HH:MM. */
  now: string;
  current_sim_date: string;
  epoch_start_date: string | null;
  epoch_end_date: string | null;
  paused: boolean;
  /// True while the clean-reset path is mid-flight (audit_log
  /// truncate + boss-rebuild-all replay). The SimClockBadge
  /// renders a spinner instead of the Restart button while this
  /// is true. Defaults to false on backends that don't track it.
  restart_in_progress?: boolean;
}>;
