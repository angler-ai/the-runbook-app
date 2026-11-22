const roles = ['api', 'scheduler', 'worker', 'vault'];

export function loadMigrationConfig(env = process.env) {
  const { RUNNER_MIGRATE_DATABASE_URL: url, RUNNER_GROUP_ID: group,
    RUNNER_ID: runner, RUNNER_CAPACITY: rawCapacity, RUNNER_VERSION: version } = env;
  const capacity = Number(rawCapacity);
  const protocol = URL.canParse(url) && new URL(url).protocol;
  if (!['postgres:', 'postgresql:'].includes(protocol) || !/^rg_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(group) ||
      !/^rn_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(runner) ||
      !Number.isInteger(capacity) || capacity < 1 || capacity > 1000 || !version) {
    throw new Error('runner install configuration is missing or invalid');
  }
  const passwords = Object.fromEntries(roles.map((role) => [role, env[`RUNNER_DB_${role.toUpperCase()}_PASSWORD`]]));
  if (Object.values(passwords).some((password) => !/^[a-f0-9]{64}$/.test(password))) {
    throw new Error('runner database login configuration is missing or invalid');
  }
  return { url, group, runner, capacity, version, passwords };
}
