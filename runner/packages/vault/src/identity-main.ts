import { required } from '@runbook/runner-shared/config';
import { openPool } from '@runbook/runner-shared/db';
import { VaultIdentity } from './identity.ts';
import { createKek } from './kek/index.ts';

/** Print the public key using only vault DB and KEK settings, before registration. */
export async function main(): Promise<void> {
  const args = process.argv.slice(3);
  if (args.length > 1 || (args.length === 1 && args[0] !== '--rotate')) throw new Error('Usage: runner vault-identity [--rotate]');
  const provider = process.env.RUNNER_KEK_PROVIDER;
  if (provider !== 'local' && provider !== 'gcp-kms') throw new Error('RUNNER_KEK_PROVIDER must be local or gcp-kms');
  const kek = await createKek(provider === 'local'
    ? { provider, file: required(process.env.RUNNER_KEK_LOCAL_FILE, 'RUNNER_KEK_LOCAL_FILE') }
    : { provider, key: required(process.env.RUNNER_KEK_GCP_KEY, 'RUNNER_KEK_GCP_KEY') });
  const pool = openPool('vault', required(process.env.RUNNER_DATABASE_URL, 'RUNNER_DATABASE_URL'));
  try {
    const identity = await (args.length ? VaultIdentity.rotate(pool, kek) : VaultIdentity.load(pool, kek));
    process.stdout.write(`${identity.publicKey}\n`);
  } finally {
    await pool.end();
  }
}
