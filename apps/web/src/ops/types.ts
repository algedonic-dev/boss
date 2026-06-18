// Cybernetics observability types — port of apps/web/src/types.ts.

export type VmResult<T = unknown> = {
  vm_id: string;
  status: number;
  body: T;
  error?: string;
};

export type HealthBody = {
  vm_id: string;
  status: string;
  timestamp: string;
};

export type AgentSpec = {
  id: string;
  display_name: string;
  system_prompt: string;
  model: string;
  hourly_budget_usd_micros: number;
  max_concurrent_runs: number;
};

export type QueueEntry = {
  agent: string;
  depth: number;
};

export type RunHandle = {
  id: string;
  agent: string;
  started_at: string;
};

export type CostEntry = {
  agent: string;
  cost: {
    input_tokens: number;
    output_tokens: number;
    usd_micros: number;
  };
  window: 'hour' | 'day';
};

export type Snapshot = {
  /// True when the deployment is configured to emit synthetic
  /// agent/queue/run/cost data (i.e. the brewery playground's
  /// `[demo_agents]` config in `infra/observability/config.toml`).
  /// Used to render the demo-mode banner on /ops so a visitor
  /// doesn't mistake the synthetic surface for a live cybernetics
  /// deployment.
  demo_mode?: boolean;
  health: ReadonlyArray<VmResult<HealthBody>>;
  agents: ReadonlyArray<VmResult<ReadonlyArray<AgentSpec>>>;
  queues: ReadonlyArray<VmResult<ReadonlyArray<QueueEntry>>>;
  runs: ReadonlyArray<VmResult<ReadonlyArray<RunHandle>>>;
  costs: ReadonlyArray<VmResult<ReadonlyArray<CostEntry>>>;
};

export type TelemetryEvent = {
  id: string;
  timestamp: string;
  source: string;
  kind: string;
  payload: unknown;
};

export const TELEMETRY_KINDS = [
  'cybernetics.message.enqueued',
  'cybernetics.message.rejected',
  'cybernetics.dispatch.requested',
  'cybernetics.dispatch.started',
  'cybernetics.dispatch.denied',
  'cybernetics.dispatch.skipped',
  'cybernetics.dispatch.completed',
  'cybernetics.cost.recorded',
] as const;
