// The vault (design §5.1 zone R3, §7): the only process that can use the
// credential KEK, open secret versions and mint material. It takes no
// traffic from sandboxes, loads no plugins, and hands material only to the
// gateway, for live releases, never in a response.
import type pg from 'pg';
import type { IngestKeyResponse } from '@runbook/runner-shared/contracts/internal';
import { AssertionVerifier, type AssertionOptions } from '@runbook/runner-shared/user-assertion';
import { createLogger, type Logger } from '@runbook/runner-shared/log';
import { CoreGrants, type CoreOptions } from './core.ts';
import { GatewayClient, type GatewayOptions } from './gateway.ts';
import { VaultIdentity } from './identity.ts';
import { IngestKeys, publicKeyFingerprint } from './ingest.ts';
import type { Kek } from './kek/index.ts';
import { loadOrCreateKey } from './keys.ts';
import { rewrapVaultKeys } from './rewrap.ts';
import { OAuthAccessService } from './oauth/access.ts';
import { TunneledOAuthProvider, type OAuthProvider } from './oauth/provider.ts';
import { ReleaseService } from './releases.ts';
import type { SecretManager } from './secret-manager.ts';
import { VertexIdentity } from './vertex-identity.ts';
import { SecretStore } from './secrets.ts';

export interface VaultOptions {
  pool: pg.Pool;
  kek: Kek;
  orgId: string;
  now?: () => Date;
  log?: Logger;
  assertion?: Pick<AssertionOptions, 'coreUrl' | 'jwksBaseUrl' | 'installId' | 'fetch'>;
  /** The core (grants, JWKS) and the gateway (material); without them only sealing works. */
  core?: CoreOptions;
  gateway?: GatewayOptions;
  oauthProvider?: OAuthProvider;
  vertexIdentity?: VertexIdentity;
  secretManager?: Pick<SecretManager, 'access' | 'assertAllowed'>;
  /** How often to renew grants and end revoked releases (ms); 0 disables the timers (tests). */
  renewIntervalMs?: number;
}

type Resolved = Required<Pick<VaultOptions, 'pool' | 'kek' | 'orgId' | 'now' | 'log'>> & VaultOptions;

export class Vault {
  readonly pool: pg.Pool;
  readonly kek: Kek;
  readonly orgId: string;
  readonly now: () => Date;
  readonly log: Logger;
  readonly identity: VaultIdentity;
  readonly ingest: IngestKeys;
  readonly secrets: SecretStore;
  readonly assertions: AssertionVerifier | undefined;
  readonly oauth: OAuthAccessService | undefined;
  readonly releases: ReleaseService | undefined;
  private rotating?: Promise<number>;
  private readonly timers: NodeJS.Timeout[] = [];

  readonly vertex: VertexIdentity;
  readonly installId?: string;

  private constructor(options: Resolved, displayKey: Buffer, identity: VaultIdentity) {
    this.pool = options.pool;
    this.identity = identity;
    this.installId = options.assertion?.installId;
    this.kek = options.kek;
    this.orgId = options.orgId;
    this.now = options.now;
    this.log = options.log;
    this.vertex = options.vertexIdentity ?? new VertexIdentity({ now: this.now });
    this.ingest = new IngestKeys(this.pool, this.kek, this.now);
    this.secrets = new SecretStore({
      pool: this.pool,
      kek: this.kek,
      orgId: this.orgId,
      displayKey,
      secretManager: options.secretManager,
      now: this.now,
      log: this.log,
    });
    this.assertions = options.assertion && new AssertionVerifier({
      ...options.assertion,
      orgId: this.orgId,
      now: () => this.now().getTime(),
      consumeReplay: async (install, jti, expiresAt) => {
        const inserted = await this.pool.query(
          `INSERT INTO vault_assertion_replays (install_id, jti, expires_at) VALUES ($1, $2, $3)
           ON CONFLICT (install_id, jti) DO NOTHING`, [install, jti, expiresAt],
        );
        return inserted.rowCount === 1;
      },
    });
    const gateway = options.gateway && new GatewayClient(options.gateway);
    this.oauth = gateway && options.gateway ? new OAuthAccessService({
      pool: options.pool, gateway, secrets: this.secrets, now: this.now,
      identity: this.identity, installId: this.installId, log: this.log,
      provider: options.oauthProvider ?? new TunneledOAuthProvider(options.gateway.baseUrl, options.gateway.token),
    }) : undefined;
    this.releases =
      options.core && gateway
        ? new ReleaseService({
            pool: this.pool,
            core: new CoreGrants(options.core, this.now),
            gateway,
            oauth: this.oauth,
            vertex: this.vertex,
            secrets: this.secrets,
            now: this.now,
            log: this.log,
          })
        : undefined;
  }

  static async create(options: VaultOptions): Promise<Vault> {
    const full: Resolved = {
      now: () => new Date(),
      log: createLogger('vault'),
      ...options,
    };
    const displayKey = await loadOrCreateKey(full.pool, full.kek, 'fingerprint');
    const identity = await VaultIdentity.load(full.pool, full.kek);
    const vault = new Vault(full, displayKey, identity);
    await vault.oauth?.restoreDiscoveryCandidates(vault.orgId);
    const key = await vault.ingest.current();
    // Operators compare this with `runbook runner credential set --pin` (design §7.4).
    vault.log.info('ingest key ready', {
      ingest_key_id: key.ingest_key_id,
      fingerprint: publicKeyFingerprint(key),
      expires_at: key.expires_at,
      kek: vault.kek.describe(),
      identity_public_key: identity.publicKey,
    });
    const interval = options.renewIntervalMs ?? 5_000;
    if (vault.releases && interval > 0) {
      const live = await vault.releases.renewer.loadLive();
      if (live > 0) vault.log.info('live releases picked up', { count: live });
      vault.every(interval, 'grant renewal', () => vault.releases!.renewer.runDue());
      vault.every(3_600_000, 'secret version cleanup', () => vault.secrets.destroyExpiredVersions());
    }
    if (vault.oauth && interval > 0) vault.every(60_000, 'OAuth keep-alive', () => vault.oauth!.maintain());
    vault.every(3_600_000, 'KEK re-wrap', () => vault.rewrapKeys());
    if (vault.assertions) vault.every(60_000, 'assertion replay cleanup', () =>
      vault.pool.query('DELETE FROM vault_assertion_replays WHERE expires_at < $1', [vault.now()]));
    return vault;
  }

  async ingestKey(): Promise<IngestKeyResponse> {
    const key = await this.ingest.current();
    return this.installId ? this.identity.attest(key, this.orgId, this.installId) : key;
  }

  rewrapKeys(): Promise<number> {
    this.rotating ??= rewrapVaultKeys(this.pool, this.kek, this.orgId, this.log).finally(() => {
      this.rotating = undefined;
    });
    return this.rotating;
  }

  /** Run `fn` every `ms`, logging (not throwing) failures; stopped by `stop`. */
  every(ms: number, name: string, fn: () => Promise<unknown>): void {
    const timer = setInterval(() => {
      fn().catch((e: unknown) => this.log.error(`${name} failed`, { error: e instanceof Error ? e.name : 'error' }));
    }, ms);
    timer.unref();
    this.timers.push(timer);
  }

  stop(): void {
    for (const t of this.timers) clearInterval(t);
    this.timers.length = 0;
  }
}
