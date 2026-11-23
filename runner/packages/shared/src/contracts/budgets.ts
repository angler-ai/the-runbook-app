export interface Budget {
  scope: 'org' | 'group' | 'environment' | 'project' | 'agent' | 'run' | 'step';
  id: string;
  name: string;
  project_id: string | null;
  period: 'day' | 'month' | 'run' | 'step' | 'concurrency';
  cap: number;
  spend: number;
  percentage: number | null;
  alert_threshold_percent: number | null;
  daily_spend: { date: string; spend_usd: number }[];
  blocked_count?: number;
  execution_id?: string;
  blocked: { step_run_id: string; reason: string; execution_id?: string; title?: string }[];
}
