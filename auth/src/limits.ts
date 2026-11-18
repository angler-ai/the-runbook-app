// Send-code limits (PRD §18): a 60 s gap and an hourly cap per email address, and
// an hourly cap per client IP. Keys are SHA-256 hashes, so the table holds no
// addresses. One conditional upsert per key: on D1 there are no interactive
// transactions, and on Postgres the row lock on conflict serializes racers.
import { and, eq, gte, lt, sql } from 'drizzle-orm';
import type * as pgSchema from './schema/pg';
import type * as sqliteSchema from './schema/sqlite';

export const LIMITS = {
  /** Minimum time between two codes for one address. */
  emailGapMs: 60_000,
  /** Codes per address per hour. */
  emailPerHour: 5,
  /**
   * Gate fix 7: codes per hour for an address from a client address that signed
   * in with it before (within `knownForMs`), counted apart from everyone else's,
   * so a stranger who uses up the address's allowance cannot lock its owner out.
   */
  knownEmailPerHour: 5,
  knownForMs: 30 * 24 * 3_600_000,
  /** Send-code requests per client IP per hour (Shelf's number). */
  ipPerHour: 10,
  windowMs: 3_600_000,
  /** MCP2 review fix 5: OAuth client registrations per client address per day. */
  registrationsPerDay: 20,
  dayMs: 24 * 3_600_000,
  /** A registered client that never completed a grant is deleted after this. */
  idleClientMs: 7 * 24 * 3_600_000,
} as const;

export interface AuthStore {
  /** True when the send may proceed; false when a limit refuses it. The window defaults to an hour. */
  claim(key: string, nowMs: number, gapMs: number, max: number, windowMs?: number): Promise<boolean>;
  /**
   * MCP2 review fix 5: deletes OAuth clients registered before `beforeMs` that
   * never completed a grant (no consent, access or refresh token). Their
   * resource links go with them (foreign-key cascade).
   */
  sweepIdleClients(beforeMs: number): Promise<void>;
  /** Voids an undelivered code (Shelf: an unsent code must not stay usable). */
  deleteVerification(identifier: string): Promise<void>;
  /** Gate fix 7: remembers that `key` (a `known:` key) signed in at `nowMs`. */
  markKnown(key: string, nowMs: number): Promise<void>;
  /** True when `key` was marked at or after `sinceMs`. */
  known(key: string, sinceMs: number): Promise<boolean>;
}

type Names = 'sendLimit' | 'verification' | 'oauthClient' | 'oauthConsent' | 'oauthAccessToken' | 'oauthRefreshToken';
type Tables = Pick<typeof sqliteSchema, Names> | Pick<typeof pgSchema, Names>;

/**
 * The same Drizzle statements on both dialects. `db` is a DrizzleD1Database or a
 * PostgresJsDatabase; their builder APIs agree on everything used here.
 */
export function drizzleStore(db: any, tables: Tables): AuthStore {
  const t = tables.sendLimit;
  return {
    async claim(key, nowMs, gapMs, max, windowMs = LIMITS.windowMs) {
      const windowOpen = sql`${t.windowStart} <= ${nowMs - windowMs}`;
      const rows: unknown[] = await db
        .insert(t)
        .values({ key, windowStart: nowMs, count: 1, lastSent: nowMs })
        .onConflictDoUpdate({
          target: t.key,
          set: {
            count: sql`CASE WHEN ${windowOpen} THEN 1 ELSE ${t.count} + 1 END`,
            windowStart: sql`CASE WHEN ${windowOpen} THEN ${nowMs} ELSE ${t.windowStart} END`,
            lastSent: nowMs,
          },
          setWhere: sql`${t.lastSent} <= ${nowMs - gapMs} AND (${windowOpen} OR ${t.count} < ${max})`,
        })
        .returning({ key: t.key });
      return rows.length > 0;
    },
    async sweepIdleClients(beforeMs) {
      const c = tables.oauthClient;
      const unused = (table: typeof tables.oauthConsent | typeof tables.oauthAccessToken | typeof tables.oauthRefreshToken) =>
        sql`NOT EXISTS (SELECT 1 FROM ${table} WHERE ${table.clientId} = ${c.clientId})`;
      await db
        .delete(c)
        .where(and(lt(c.createdAt, new Date(beforeMs)), unused(tables.oauthConsent), unused(tables.oauthAccessToken), unused(tables.oauthRefreshToken)));
    },
    async deleteVerification(identifier) {
      await db.delete(tables.verification).where(eq(tables.verification.identifier, identifier));
    },
    // Marks share the send_limit table (no schema change): count 0, last_sent the
    // sign-in time. Their `known:` keys never meet a claim's `ip:`/`email:` keys.
    async markKnown(key, nowMs) {
      await db
        .insert(t)
        .values({ key, windowStart: nowMs, count: 0, lastSent: nowMs })
        .onConflictDoUpdate({ target: t.key, set: { lastSent: nowMs } });
    },
    async known(key, sinceMs) {
      const rows: unknown[] = await db
        .select({ key: t.key })
        .from(t)
        .where(and(eq(t.key, key), gte(t.lastSent, sinceMs)))
        .limit(1);
      return rows.length > 0;
    },
  };
}

/**
 * The per-IP limit's key for a client address, as the host reported it in its
 * client-address header (Rust's `x-runbook-client-ip`, Cloudflare's
 * `cf-connecting-ip`; nothing the client sent): an IPv4 address (or an
 * IPv4-mapped IPv6 one) as itself, an IPv6 address by its /64, which one
 * client controls whole (the Rust hosts and Better Auth's own limiter do the
 * same), anything else `unknown`.
 */
export function clientKey(raw: string | null | undefined): string {
  const ip = (raw ?? '').trim();
  const v4 = (s: string) => /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.test(s) && s.split('.').every((o) => Number(o) <= 255 && String(Number(o)) === o);
  if (v4(ip)) return ip;
  if (!ip.includes(':') || ip.length > 64 || !/^[0-9a-fA-F:.]+$/.test(ip)) return 'unknown';
  let host: string;
  try {
    host = new URL(`http://[${ip}]/`).hostname.slice(1, -1);
  } catch {
    return 'unknown';
  }
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
  if (mapped) {
    const [hi, lo] = [parseInt(mapped[1], 16), parseInt(mapped[2], 16)];
    return `${hi >> 8}.${hi & 255}.${lo >> 8}.${lo & 255}`;
  }
  // Expand `::` to the eight groups, keep the first four.
  const [head, tail] = host.includes('::') ? host.split('::') : [host, undefined];
  const left = head ? head.split(':') : [];
  const right = tail ? tail.split(':') : [];
  const groups = tail === undefined ? left : [...left, ...Array(8 - left.length - right.length).fill('0'), ...right];
  if (groups.length !== 8) return 'unknown';
  return `${groups.slice(0, 4).map((g) => parseInt(g, 16).toString(16)).join(':')}::/64`;
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
}
