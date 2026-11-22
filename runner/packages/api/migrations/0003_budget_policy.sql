ALTER TABLE policy
  ADD COLUMN project_limits jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(project_limits) = 'object'),
  ADD COLUMN run_max_cost_usd numeric(14, 6) CHECK (run_max_cost_usd >= 0);
