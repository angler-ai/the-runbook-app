export function retentionConfig(env: NodeJS.ProcessEnv = process.env) {
  function days(name: string, fallback: string): number | null {
    const value = env[name] ?? fallback;
    if (value === 'off') return null;
    if (!/^\d{1,5}$/.test(value) || Number(value) < 1 || Number(value) > 36_500) {
      throw new Error(`${name} must be a whole number of days from 1 to 36500, or off`);
    }
    return Number(value);
  }

  const liveDays = days('RUNNER_RETENTION_LIVE_DAYS', '30');
  const archiveDays = days('RUNNER_RETENTION_ARCHIVE_DAYS', 'off');
  if (archiveDays !== null && (liveDays === null || archiveDays < liveDays)) {
    throw new Error('RUNNER_RETENTION_ARCHIVE_DAYS must be off or at least RUNNER_RETENTION_LIVE_DAYS (which must be enabled)');
  }
  return { liveDays, archiveDays, location: env.RUNNER_RETENTION_ARCHIVE_STORE ?? '/var/lib/runbook/runner-archives' };
}

export type RetentionConfig = ReturnType<typeof retentionConfig>;
