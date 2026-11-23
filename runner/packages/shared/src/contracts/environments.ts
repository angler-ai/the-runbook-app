import type { AgentCeiling } from './core.ts';
import type { SandboxSize } from './internal.ts';

export interface EnvironmentBudgets {
  cost_per_day_usd?: number | null;
  cost_per_month_usd?: number | null;
  concurrency?: number | null;
  max_step_cost_usd?: number | null;
  max_run_cost_usd?: number | null;
  alert_threshold_percent?: number | null;
}

export interface EnvironmentConfig {
  image: string | null;
  setup: string | null;
  setup_timeout_s: number;
  env: Record<string, string>;
  credential_env: Record<string, string>;
  size: SandboxSize;
  timeout_s: number;
  egress: AgentCeiling['hosts'] | null;
  budgets: EnvironmentBudgets;
}

export interface Environment extends EnvironmentConfig {
  id: string;
  name: string;
  description: string;
  version: number;
  version_id: string;
  is_default: boolean;
  image_digest: string | null;
  image_approved: boolean;
  created_at: string;
  updated_at: string;
}

export interface SandboxEnvironment {
  image: string | null;
  custom: boolean;
  size: SandboxSize;
  env: Record<string, string>;
  credential_names: string[];
  setup: string | null;
  setup_timeout_s: number;
}
