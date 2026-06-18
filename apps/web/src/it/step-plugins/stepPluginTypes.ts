// Mirror of boss_jobs::step_plugins::StepPluginSpec.

import type { JobKindStatus } from '../../job-kinds/jobKindTypes';

export type StepPluginSpec = {
  kind: string;
  version: number;
  status: JobKindStatus;
  label: string;
  description: string | null;
  category: string;
  metadata_schema: Record<string, unknown>;
  frontend_url: string;
  owning_team: string;
  authoring_job_id: string | null;
  created_at: string;
};
