// HPKE ingest keys (design §7.4): X25519 key pairs whose private halves are
// KEK-wrapped in ingest_keys. The newest key is served (through runner-api
// and the core proxy) until it is 30 days old, then a new one is made; each
// key opens blobs until it expires a day after that, so a blob sealed just
// before rotation still lands.
import { createHash, randomBytes } from 'node:crypto';
import type pg from 'pg';
import type { IngestKeyResponse } from '@runbook/runner-shared/contracts/internal';
import { fromBase64Url, toBase64Url } from '@runbook/runner-shared/encoding';
import { sealSuite } from '@runbook/runner-shared/seal-client';
import type { Kek } from './kek/index.ts';

const DAY = 24 * 3600_000;
export const INGEST_ROTATE_AFTER_MS = 30 * DAY;
export const INGEST_LIFETIME_MS = 31 * DAY;

type PrivateKey = Awaited<ReturnType<ReturnType<typeof sealSuite>['kem']['deserializePrivateKey']>>;

interface Row {
  id: string;
  public_key: Buffer;
  wrapped_private_key: Buffer;
  kek_ref: string;
  rotate_after: Date;
  expires_at: Date;
}

export class IngestKeys {
  private readonly pool: pg.Pool;
  private readonly kek: Kek;
  private readonly now: () => Date;
  private readonly cache = new Map<string, { key: PrivateKey; expiresAt: number }>();

  constructor(pool: pg.Pool, kek: Kek, now: () => Date) {
    this.pool = pool;
    this.kek = kek;
    this.now = now;
  }

  private ctx(id: string) {
    return { purpose: 'ingest-key', ingest_key_id: id };
  }

  /** The key to publish: the newest one, made fresh when it is due for rotation. */
  async current(): Promise<IngestKeyResponse> {
    const now = this.now();
    const { rows } = await this.pool.query<Row>(
      `SELECT id, public_key, expires_at FROM ingest_keys WHERE rotate_after > $1 ORDER BY created_at DESC LIMIT 1`,
      [now],
    );
    const row = rows[0] ?? (await this.create(now));
    return {
      ingest_key_id: row.id,
      public_key: toBase64Url(row.public_key),
      expires_at: row.expires_at.toISOString(),
    };
  }

  private async create(now: Date): Promise<Row> {
    const suite = sealSuite();
    const pair = await suite.kem.generateKeyPair();
    const publicKey = Buffer.from(await suite.kem.serializePublicKey(pair.publicKey));
    const privateKey = Buffer.from(await suite.kem.serializePrivateKey(pair.privateKey));
    const id = `ik_${randomBytes(12).toString('hex')}`;
    try {
      const { wrapped, kekRef } = await this.kek.wrap(privateKey, this.ctx(id));
      const row = {
        id,
        public_key: publicKey,
        wrapped_private_key: wrapped,
        kek_ref: kekRef,
        rotate_after: new Date(now.getTime() + INGEST_ROTATE_AFTER_MS),
        expires_at: new Date(now.getTime() + INGEST_LIFETIME_MS),
      };
      await this.pool.query(
        `INSERT INTO ingest_keys (id, public_key, wrapped_private_key, kek_ref, created_at, rotate_after, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)`,
        [row.id, row.public_key, row.wrapped_private_key, row.kek_ref, now, row.rotate_after, row.expires_at],
      );
      return row;
    } finally {
      privateKey.fill(0);
    }
  }

  /** The private key for `id`, if it exists and hasn't expired. */
  async privateKey(id: string): Promise<PrivateKey | undefined> {
    const now = this.now().getTime();
    const cached = this.cache.get(id);
    if (cached) return cached.expiresAt > now ? cached.key : undefined;
    const { rows } = await this.pool.query<Row>(
      `SELECT id, wrapped_private_key, kek_ref, expires_at FROM ingest_keys WHERE id = $1`,
      [id],
    );
    const row = rows[0];
    if (!row || row.expires_at.getTime() <= now) return undefined;
    const raw = await this.kek.unwrap(row.wrapped_private_key, row.kek_ref, this.ctx(id));
    try {
      const key = await sealSuite().kem.deserializePrivateKey(raw);
      this.cache.set(id, { key, expiresAt: row.expires_at.getTime() });
      return key;
    } finally {
      raw.fill(0);
    }
  }
}

/** SHA-256 of the raw public key, hex: logged at start so the CLI can pin it (design §7.4). */
export function publicKeyFingerprint(key: IngestKeyResponse): string {
  return createHash('sha256').update(fromBase64Url(key.public_key)).digest('hex');
}
