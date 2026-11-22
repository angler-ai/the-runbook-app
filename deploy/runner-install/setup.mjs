#!/usr/bin/env node
// Run on the Compose host. Reads the bootstrap token from stdin and writes
// untracked configuration in a private directory. No secret goes to stdout.
import { randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import { chmod, mkdir, readFile, writeFile, rename, unlink } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { loadMigrationConfig } from './migration-config.mjs';
import { loadConfig, loadInstallId, RUNNER_ROLES } from '../../runner/packages/shared/src/config.ts';

import { composeIdentitySeed, vaultIdentityFingerprint, vaultIdentityPrivateKey, vaultIdentityPublicKey }
  from '../../runner/packages/shared/src/vault-identity.ts';

const directory = new URL('../runner-config/', import.meta.url);
const file = (name) => new URL(name, directory);
const random = () => randomBytes(32).toString('hex');
const tokenValue = (value) => typeof value === 'string' && /^[A-Za-z0-9_-]{32,128}$/.test(value);
const apiInternal = 'http://runner-api:8094';
const identityReminder = 'No vault identity enrollment is recorded by this installer. Check runbook runner status; ' +
  'if no fingerprint is shown, ask an organization owner to enroll the key with runbook runner vault-identity before entering secrets.';

function endpoint(value, name, allowHttp = false) {
  let url;
  try { url = new URL(value); } catch { throw new Error(`Set ${name} to an HTTPS base URL`); }
  if ((url.protocol !== 'https:' && !(allowHttp && url.protocol === 'http:')) ||
      url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw new Error(`Invalid base URL in ${name}`);
  }
  return url.origin;
}

function groupId() {
  const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  let value = BigInt('0x' + randomBytes(16).toString('hex'));
  let id = '';
  for (let i = 0; i < 26; i++, value >>= 5n) id = alphabet[Number(value & 31n)] + id;
  return 'rg_' + id;
}

async function save(name, data) {
  const temp = file(`.${name}.new`);
  await writeFile(temp, data, { mode: 0o600 });
  await rename(temp, file(name));
}

async function readEnv(name) {
  const lines = (await readFile(file(`${name}.env`), 'utf8')).split(/\r?\n/);
  return Object.fromEntries(lines.filter((line) => line.trim() && !line.trimStart().startsWith('#')).map((line) => {
    const split = line.indexOf('=');
    if (split < 1) throw new Error(`Invalid entry in ${name}.env`);
    return [line.slice(0, split), line.slice(split + 1)];
  }));
}

async function writeEnvs(environments) {
  const install = loadInstallId(environments.api);
  if (install !== loadInstallId(environments.vault)) throw new Error('Runner API and vault installation IDs differ');
  for (const role of RUNNER_ROLES) loadConfig(role, environments[role]);
  loadMigrationConfig(environments.migrate);
  const postgres = environments.postgres;
  if (!postgres.POSTGRES_USER || !postgres.POSTGRES_DB || !postgres.POSTGRES_PASSWORD) {
    throw new Error('Postgres configuration is missing credentials');
  }
  for (const [name, values] of Object.entries(environments)) {
    if (Object.values(values).some((value) => value == null || /[\r\n]/.test(value))) {
      throw new Error(`Invalid runner environment value in ${name}.env`);
    }
  }
  for (const [name, values] of Object.entries(environments)) {
    await save(`${name}.env`, Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join(''));
  }
}

async function upgrade(core, org, version, sandboxImage) {
  const environments = Object.fromEntries(await Promise.all(
    [...RUNNER_ROLES, 'migrate', 'postgres'].map(async (role) => [role, await readEnv(role)]),
  ));
  const { api, vault, migrate } = environments;
  if (api.CORE_URL !== core || api.RUNNER_ORG_ID !== org) {
    throw new Error('Upgrade with the original core URL and organization');
  }
  api.RUNNER_INTERNAL_LISTEN = '0.0.0.0:8094';
  vault.RUNNER_INSTALL_ID = api.RUNNER_INSTALL_ID;
  vault.RUNNER_ALLOWED_RUNNER_IDS = api.RUNNER_ALLOWED_RUNNER_IDS;
  migrate.RUNNER_VERSION = version;
  for (const role of ['worker', 'gateway', 'sandbox-ctl']) environments[role].API_URL = apiInternal;
  if (sandboxImage) {
    for (const role of ['worker', 'sandbox-ctl']) environments[role].RUNNER_SANDBOX_IMAGE = sandboxImage;
  }
  await writeEnvs(environments);
  console.log('Runner configuration upgraded.');
  if (!existsSync(file('vault-identity-enrolled'))) console.log(identityReminder);
}

const registrationRecovery = 'Invalid core registration reply. Run runbook runner status for this organization. ' +
  'If it shows this installation, ask the core operator to remove the orphaned registration as described in the Compose recovery instructions. ' +
  'Then run runbook runner bootstrap and repeat setup with the original URLs.';

async function jsonObject(read, message) {
  try {
    const value = await read();
    if (value && typeof value === 'object' && !Array.isArray(value)) return value;
  } catch {
    // JSON parser errors may contain credentials from the input.
  }
  throw new Error(message);
}

async function post(core, path, token, body) {
  const response = await fetch(core + path, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(10_000),
    redirect: 'error',
  });
  if (!response.ok) throw new Error(`Core rejected ${path} (HTTP ${response.status})`);
  return jsonObject(() => response.json(), path === '/v1/runner-installs/register'
    ? registrationRecovery : 'Invalid core runner join reply. Repeat setup with the original arguments to resume registration.');
}

async function setup() {
  const args = process.argv.slice(2);
  const upgrading = args[0] === '--upgrade';
  if (upgrading) args.shift();
  if (args.length !== 4) throw new Error('Usage: node deploy/runner-install/setup.mjs [--upgrade] CORE_URL RUNNER_HTTPS_URL ORG_ID VERSION');
  const [coreInput, runnerInput, org, version] = args;
  const insecureCore = process.env.RUNNER_INSECURE_CORE_HTTP === 'true';
  const core = endpoint(coreInput, 'CORE_URL', insecureCore);
  const runnerUrl = endpoint(runnerInput, 'RUNNER_HTTPS_URL');
  if (!/^org_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(org) || !/^[A-Za-z0-9_.+-]{1,64}$/.test(version)) {
    throw new Error('Invalid organization ID or runner version');
  }
  const capacity = Number(process.env.RUNNER_CAPACITY ?? '1');
  if (!Number.isInteger(capacity) || capacity < 1 || capacity > 1000) throw new Error('Invalid RUNNER_CAPACITY');
  const sandboxImage = process.env.RUNNER_SANDBOX_IMAGE;
  const image = sandboxImage ?? 'runbook-sandbox:1';
  if (!/^[A-Za-z0-9._/@:-]+$/.test(image)) throw new Error('Invalid RUNNER_SANDBOX_IMAGE');
  const project = process.env.COMPOSE_PROJECT_NAME ?? 'runbook';
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(project)) throw new Error('Invalid COMPOSE_PROJECT_NAME');

  await mkdir(directory, { mode: 0o700, recursive: true });
  await chmod(directory, 0o700);
  if (upgrading) {
    if (!existsSync(file('complete'))) throw new Error('No completed Compose runner installation to upgrade');
    await upgrade(core, org, version, sandboxImage);
    return;
  }
  if (existsSync(file('complete'))) throw new Error('Runner configuration already exists. Use --upgrade to refresh its environments.');
  if (!existsSync(file('kek.json'))) {
    await save('kek.json', JSON.stringify({
      format: 'runbook-runner-local-kek/1', key_id: randomBytes(8).toString('hex'), bootstrap_identity: true,
      current: 1, keys: { 1: randomBytes(32).toString('base64') },
    }));
  }
  const resume = file('registration.json');
  let record;
  if (existsSync(resume)) {
    record = await jsonObject(async () => JSON.parse(await readFile(resume, 'utf8')),
      'Invalid registration.json. Restore the private resume file from backup or follow the Compose registration recovery instructions.');
    if (record.core !== core || record.runnerUrl !== runnerUrl || record.org !== org) {
      throw new Error('Resume setup with the original core URL, runner URL and organization');
    }
  } else {
    if (process.stdin.isTTY) throw new Error('Pipe the bootstrap token from runbook runner bootstrap');
    process.stdin.setEncoding('utf8');
    let token = '';
    for await (const part of process.stdin) {
      token += part;
      if (token.length > 128) throw new Error('Expected only the runner bootstrap token on stdin');
    }
    token = token.trim();
    if (!/^trb_rbs_[A-Za-z0-9_-]{43}$/.test(token)) throw new Error('Expected a runner bootstrap token on stdin');
    const response = await fetch(core + '/v1/meta', { redirect: 'error', signal: AbortSignal.timeout(10_000) });
    if (!response.ok) throw new Error('Core runner contract unavailable');
    const { runner_contract_version: contract } = await jsonObject(() => response.json(), 'Invalid core metadata reply');
    if (!Number.isSafeInteger(contract) || contract < 4) {
      throw new Error('Runner needs core runner contract version 4 or later. Upgrade the core first.');
    }
    const kek = await jsonObject(async () => JSON.parse(await readFile(file('kek.json'), 'utf8')), 'Invalid kek.json');
    if (kek.bootstrap_identity !== true) throw new Error('Existing KEK lacks a Compose bootstrap identity');
    const initialKey = Buffer.from(kek.keys?.[1] ?? '', 'base64');
    if (initialKey.length !== 32) throw new Error('Invalid initial Compose KEK');
    const seed = composeIdentitySeed(initialKey);
    initialKey.fill(0);
    let publicKey;
    try {
      publicKey = vaultIdentityPublicKey(vaultIdentityPrivateKey(seed));
    } finally {
      seed.fill(0);
    }
    const registration = await post(core, '/v1/runner-installs/register', token, {
      url: runnerUrl, version, contract_version: 4, vault_identity_key: publicKey,
    });
    const { install_id, directory_token, release_token, join_token } = registration;
    if (typeof install_id !== 'string' || !/^ri_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(install_id) ||
        ![directory_token, release_token, join_token].every(tokenValue)) {
      throw new Error(registrationRecovery);
    }
    record = { core, runnerUrl, org, group: groupId(), install_id, directory_token, release_token, join_token,
      vault_identity_key: publicKey };
    await save('registration.json', JSON.stringify(record));
  }
  const install = record.install_id;
  if (!record.runner_id) {
    const { runner_id, claim_token } = await post(core, `/v1/runner-installs/${install}/runners`, record.join_token, {
      name: 'docker', group_id: record.group, group_name: 'default', version,
    });
    if (typeof runner_id !== 'string' || !/^rn_[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(runner_id) || !tokenValue(claim_token)) {
      throw new Error('Invalid core runner join reply. Repeat setup with the original arguments to resume registration.');
    }
    Object.assign(record, { runner_id, claim_token });
    await save('registration.json', JSON.stringify(record));
  }
  const runner = record.runner_id;
  const roles = ['owner', 'api', 'scheduler', 'worker', 'vault'];
  const links = [
    'API_VAULT', 'API_WORKER', 'WORKER_API', 'WORKER_VAULT', 'WORKER_GATEWAY', 'WORKER_SANDBOXCTL',
    'SANDBOXCTL_API', 'SANDBOXCTL_GATEWAY', 'SANDBOXCTL_VAULT', 'VAULT_GATEWAY', 'GATEWAY_API',
  ];
  const tokens = Object.fromEntries(links.map((link) => [link, random()]));
  const passwords = Object.fromEntries(roles.map((role) => [role, random()]));
  const credentials = (...selected) => Object.fromEntries(selected.map((link) => [`RUNNER_TOKEN_${link}`, tokens[link]]));
  const database = (role) => `postgres://runner_${role}:${passwords[role]}@runner-postgres:5432/runner`;
  const shared = {
    CORE_URL: core, RUNNER_INSECURE_CORE_HTTP: String(insecureCore), RUNNER_INSECURE_INTERNAL_HTTP: 'true',
  };
  const identity = { RUNNER_INSTALL_ID: install, RUNNER_GROUP_ID: record.group, RUNNER_CAPACITY: capacity };
  const urls = {
    api: apiInternal, vault: 'http://runner-vault:8091', worker: 'http://runner-worker:8094',
    gateway: 'http://runner-gateway-control:8092', sandbox: 'http://runner-sandbox-ctl:8093',
  };

  const environments = {};
  environments.postgres = { POSTGRES_USER: 'runner_owner', POSTGRES_DB: 'runner', POSTGRES_PASSWORD: passwords.owner };
  environments['migrate'] = {
    ...identity, RUNNER_ID: runner, RUNNER_VERSION: version, RUNNER_MIGRATE_DATABASE_URL: database('owner'),
    ...Object.fromEntries(roles.slice(1).map((role) => [`RUNNER_DB_${role.toUpperCase()}_PASSWORD`, passwords[role]])),
  };
  environments['api'] = {
    ...shared, ...identity, RUNNER_ROLE: 'api', RUNNER_ORG_ID: org, CORE_DIRECTORY_TOKEN: record.directory_token,
    RUNNER_ALLOWED_RUNNER_IDS: runner, RUNNER_INTERNAL_LISTEN: '0.0.0.0:8094', RUNNER_DATABASE_URL: database('api'),
    RUNNER_SCHEDULER_DATABASE_URL: database('scheduler'), RUNNER_METRICS_TOKEN: random(), VAULT_URL: urls.vault,
    RUNNER_WORKER_API_TOKENS: JSON.stringify({ [runner]: tokens.WORKER_API }),
    RUNNER_WORKER_NOTIFIERS: JSON.stringify({ [runner]: { url: urls.worker, token: tokens.API_WORKER } }),
    ...credentials('API_VAULT', 'GATEWAY_API', 'SANDBOXCTL_API'),
  };
  environments['worker'] = {
    ...shared, ...identity, RUNNER_ROLE: 'worker', RUNNER_ID: runner, CORE_CLAIM_TOKEN: record.claim_token,
    RUNNER_DATABASE_URL: database('worker'), RUNNER_SANDBOX_IMAGE: image, VAULT_URL: urls.vault,
    API_URL: urls.api, GATEWAY_URL: urls.gateway, SANDBOX_CTL_URL: urls.sandbox,
    ...credentials('API_WORKER', 'WORKER_API', 'WORKER_VAULT', 'WORKER_GATEWAY', 'WORKER_SANDBOXCTL'),
  };
  environments['sandbox-ctl'] = {
    ...shared, RUNNER_ROLE: 'sandbox-ctl', RUNNER_ALLOWED_RUNNER_IDS: runner, RUNNER_SANDBOX_IMAGE: image,
    RUNNER_GATEWAY_CONTAINER: `${project}-runner-gateway`, VAULT_URL: urls.vault, API_URL: urls.api, GATEWAY_URL: urls.gateway,
    ...credentials('WORKER_SANDBOXCTL', 'SANDBOXCTL_API', 'SANDBOXCTL_GATEWAY', 'SANDBOXCTL_VAULT'),
  };
  environments['vault'] = {
    ...shared, RUNNER_INSTALL_ID: install, RUNNER_ALLOWED_RUNNER_IDS: runner,
    RUNNER_ROLE: 'vault', RUNNER_ORG_ID: org, CORE_RELEASE_TOKEN: record.release_token,
    RUNNER_DATABASE_URL: database('vault'), RUNNER_KEK_PROVIDER: 'local', RUNNER_KEK_LOCAL_FILE: '/run/runner-kek.json',
    GATEWAY_URL: urls.gateway, ...credentials('API_VAULT', 'WORKER_VAULT', 'SANDBOXCTL_VAULT', 'VAULT_GATEWAY'),
  };
  environments['gateway'] = {
    ...shared, RUNNER_ROLE: 'gateway', RUNNER_ALLOWED_RUNNER_IDS: runner, API_URL: urls.api,
    ...credentials('VAULT_GATEWAY', 'WORKER_GATEWAY', 'SANDBOXCTL_GATEWAY', 'GATEWAY_API'),
  };
  await writeEnvs(environments);
  if (record.vault_identity_key) await save('vault-identity-enrolled', '');
  await save('complete', '');
  await unlink(resume);
  if (record.vault_identity_key) console.log(`Vault identity SHA-256: ${vaultIdentityFingerprint(record.vault_identity_key)}`);
  else console.log(identityReminder);
  console.log(`Runner ${runner} registered in ${install}. Configuration is in ${fileURLToPath(directory)}.`);
}

setup().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
