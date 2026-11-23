// Send-code limits (PRD §18): a 60 s gap and an hourly cap per email address, and
// an hourly cap per client IP. Keys are SHA-256 hashes, so the table holds no
// addresses. One conditional upsert per key: on D1 there are no interactive
// transactions, and on Postgres the row lock on conflict serializes racers.
import { and, count, eq, gte, inArray, isNull, lt, or, sql } from 'drizzle-orm';
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
  pendingMfaNotifications(userId?: string): Promise<Array<{ id: string; email: string; notification: string }>>;
  markMfaNotified(id: string): Promise<void>;
  auditMfa(userId: string, action: string, actor: string): Promise<void>;
  mfaFactors(ids: string[]): Promise<import('./mfa').MfaFactors[]>;
  mfaGeneration(userId: string): Promise<number>;
  startTotp(userId: string, sessionId: string, factor: TotpSetup, fresh: boolean, expiresAt: Date): Promise<boolean>;
  confirmTotp(userId: string, sessionId: string, factor: TotpSetup, fresh: boolean): Promise<boolean>;
  resetMfa(email: string | null, ids: string[] | null, actor: string): Promise<Array<{ id: string; email: string }>>;
  removeMfa(userId: string, passkeyId: string | null, allowed: { passkeys: boolean; totp: boolean; required: boolean }): Promise<boolean>;
  claim(key: string, nowMs: number, gapMs: number, max: number, windowMs?: number, lockoutMs?: number): Promise<boolean>;
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

type TotpSetup = { secret: string; backupCodes: string };

export const totpSetupId = (sessionId: string) => `totp-setup:${sessionId}`;

type Names = 'sendLimit' | 'verification' | 'oauthClient' | 'oauthConsent' | 'oauthAccessToken' | 'oauthRefreshToken' | 'session' | 'user' | 'passkey' | 'twoFactor' | 'mfaAudit';
type Tables = Pick<typeof sqliteSchema, Names> | Pick<typeof pgSchema, Names>;

/**
 * The same Drizzle statements on both dialects. `db` is a DrizzleD1Database or a
 * PostgresJsDatabase; their builder APIs agree on everything used here.
 */
export function drizzleStore(db: any, tables: Tables): AuthStore {
  const t = tables.sendLimit;
  const { user, twoFactor, passkey, session, mfaAudit, oauthAccessToken, oauthRefreshToken } = tables;
  const event = (userId: string, action: string, actor: string, notification?: string) => ({
    id: crypto.randomUUID(), userId, action, actor, occurredAt: new Date(), notification,
  });
  const auditSelected = (tx: any, userId: string, action: string, notification: string, guard: import('drizzle-orm').SQL) =>
    tx.insert(mfaAudit).select(tx.select({
      id: sql`${crypto.randomUUID()}`, userId: sql`${userId}`, action: sql`${action}`,
      actor: sql`${userId}`, occurredAt: sql`${sql.param(new Date(), mfaAudit.occurredAt)}`,
      notification: sql`${notification}`, notifyAttemptedAt: sql`null`, notifiedAt: sql`null`,
    }).from(user).where(and(eq(user.id, userId), guard)));
  const retryableNotification = (now: Date) => and(
    isNull(mfaAudit.notifiedAt),
    or(isNull(mfaAudit.notifyAttemptedAt), lt(mfaAudit.notifyAttemptedAt, new Date(now.getTime() - 60_000))),
  );
  // D1 batches execute atomically. On Postgres the row lock serializes both
  // factor removals and reset for the same account before checking the guard.
  const apply = (queries: (tx: any) => any[], userId: string) =>
    typeof db.batch === 'function'
      ? db.batch(queries(db))
      : db.transaction(async (tx: any) => {
          await tx.select({ id: user.id }).from(user).where(eq(user.id, userId)).for('update');
          const results = [];
          for (const query of queries(tx)) results.push(await query);
          return results;
        });
  const liveSession = (userId: string, sessionId: string, fresh: boolean) => sql`EXISTS (
    SELECT 1 FROM ${session} WHERE ${session.id} = ${sessionId} AND ${session.userId} = ${userId}
      AND (NOT ${fresh} OR ${session.mfaVerifiedAt} IS NOT NULL)
      AND ${session.expiresAt} > ${sql.param(new Date(), session.expiresAt)}
  )`;
  const canEnrollTotp = (userId: string, sessionId: string, fresh: boolean) => sql`(
    ${liveSession(userId, sessionId, fresh)}
    AND (${fresh} OR NOT EXISTS (SELECT 1 FROM ${passkey} WHERE ${passkey.userId} = ${userId}))
    AND NOT EXISTS (SELECT 1 FROM ${twoFactor} WHERE ${twoFactor.userId} = ${userId} AND ${twoFactor.verified} IS TRUE)
  )`;
  return {
    async pendingMfaNotifications(userId) {
      const now = new Date();
      const candidates = await db.select({ id: mfaAudit.id, email: user.email, notification: mfaAudit.notification })
        .from(mfaAudit).innerJoin(user, eq(mfaAudit.userId, user.id))
        .where(and(retryableNotification(now), sql`${mfaAudit.notification} IS NOT NULL`, userId ? eq(user.id, userId) : undefined))
        .orderBy(mfaAudit.occurredAt).limit(25) as Array<{ id: string; email: string; notification: string }>;
      const claimed = [];
      for (const candidate of candidates) {
        const rows = await db.update(mfaAudit).set({ notifyAttemptedAt: now })
          .where(and(eq(mfaAudit.id, candidate.id), retryableNotification(now))).returning({ id: mfaAudit.id });
        if (rows.length) claimed.push(candidate);
      }
      return claimed;
    },
    async markMfaNotified(id) {
      await db.update(mfaAudit).set({ notifiedAt: new Date() }).where(eq(mfaAudit.id, id));
    },
    async auditMfa(userId, action, actor) {
      await db.insert(mfaAudit).values(event(userId, action, actor));
    },
    async mfaFactors(ids) {
      const found = [];
      // D1 permits at most 100 bound parameters per statement.
      for (let offset = 0; offset < ids.length; offset += 96) {
        const batch = ids.slice(offset, offset + 96);
        const users = await db.select({ id: user.id, email: user.email }).from(user)
          .where(inArray(user.id, batch)) as Array<{ id: string; email: string }>;
        const authenticators = await db.select({ id: twoFactor.userId }).from(twoFactor)
          .where(and(inArray(twoFactor.userId, batch), eq(twoFactor.verified, true))) as Array<{ id: string }>;
        const counts = await db.select({ id: passkey.userId, total: count() }).from(passkey)
          .where(inArray(passkey.userId, batch)).groupBy(passkey.userId) as Array<{ id: string; total: number }>;
        const verified = new Set(authenticators.map(({ id }) => id));
        const passkeys = new Map(counts.map(({ id, total }) => [id, total]));
        found.push(...users.map(({ id, email }) => ({
          auth_user_id: id, email, passkeys: passkeys.get(id) ?? 0, authenticator: verified.has(id),
        })));
      }
      return found;
    },
    async mfaGeneration(userId) {
      const rows = await db.select({ generation: user.mfaGeneration }).from(user).where(eq(user.id, userId));
      if (!rows[0]) throw new Error('Missing auth account');
      return rows[0].generation;
    },
    async startTotp(userId, sessionId, factor, fresh, expiresAt) {
      const id = crypto.randomUUID();
      const identifier = totpSetupId(sessionId);
      const verification = tables.verification;
      const guard = canEnrollTotp(userId, sessionId, fresh);
      const now = new Date();
      const result = await apply((tx) => [
        tx.delete(verification).where(and(eq(verification.identifier, identifier), guard)),
        tx.insert(verification).select(tx.select({
          id: sql`${id}`, identifier: sql`${identifier}`, value: sql`${JSON.stringify(factor)}`,
          expiresAt: sql`${sql.param(expiresAt, verification.expiresAt)}`,
          createdAt: sql`${sql.param(now, verification.createdAt)}`, updatedAt: sql`${sql.param(now, verification.updatedAt)}`,
        }).from(user).where(and(eq(user.id, userId), guard))).returning({ id: verification.id }),
        auditSelected(tx, userId, 'mfa.totp_setup_started', 'started',
          sql`EXISTS (SELECT 1 FROM ${verification} WHERE ${verification.id} = ${id})`),
      ], userId);
      return result[1].length !== 0;
    },
    async confirmTotp(userId, sessionId, factor, fresh) {
      const id = crypto.randomUUID();
      const guard = canEnrollTotp(userId, sessionId, fresh);
      const inserted = sql`EXISTS (SELECT 1 FROM ${twoFactor} WHERE ${twoFactor.id} = ${id})`;
      const result = await apply((tx) => [
        tx.delete(twoFactor).where(and(eq(twoFactor.userId, userId), sql`${twoFactor.verified} IS NOT TRUE`, guard)),
        tx.insert(twoFactor).select(tx.select({
          id: sql`${id}`, secret: sql`${factor.secret}`, backupCodes: sql`${factor.backupCodes}`,
          userId: sql`${userId}`, verified: sql`true`, failedVerificationCount: sql`0`, lockedUntil: sql`null`,
        }).from(user).where(and(eq(user.id, userId), guard)))
          .onConflictDoNothing().returning({ id: twoFactor.id }),
        tx.update(user).set({ twoFactorEnabled: true }).where(and(eq(user.id, userId), inserted)),
        auditSelected(tx, userId, 'mfa.totp_enrolled', 'enrolled', inserted),
      ], userId);
      return result[1].length !== 0;
    },
    async resetMfa(email, ids, actor) {
      const matches = email === null ? inArray(user.id, ids ?? []) : eq(user.email, email);
      const found = await db.select({ id: user.id, email: user.email }).from(user).where(matches) as Array<{ id: string; email: string }>;
      for (const { id } of found) {
        await apply((tx) => [
          tx.update(user).set({ mfaGeneration: sql`${user.mfaGeneration} + 1` }).where(eq(user.id, id)),
          tx.delete(tables.verification).where(sql`${tables.verification.identifier} IN
            (SELECT 'totp-setup:' || ${session.id} FROM ${session} WHERE ${session.userId} = ${id})`),
          tx.delete(oauthAccessToken).where(eq(oauthAccessToken.userId, id)),
          tx.delete(oauthRefreshToken).where(eq(oauthRefreshToken.userId, id)),
          tx.delete(session).where(eq(session.userId, id)),
          tx.delete(passkey).where(eq(passkey.userId, id)),
          tx.delete(twoFactor).where(eq(twoFactor.userId, id)),
          tx.update(user).set({ twoFactorEnabled: false }).where(eq(user.id, id)),
          tx.delete(t).where(or(eq(t.key, `mfa:${id}`), eq(t.key, `mfa-lock-audit:${id}`),
            ...[`mfa-attempt:${id}:`, `mfa-account-attempt:${id}:`, `mfa-lock-audit:account:${id}:`]
              .map((prefix) => sql`substr(${t.key}, 1, ${prefix.length}) = ${prefix}`))),
          tx.insert(mfaAudit).values(event(id, 'mfa.reset', actor, 'reset')),
        ], id);
      }
      return found;
    },
    async removeMfa(userId, passkeyId, { passkeys, totp, required }) {
      const otherPasskey = passkeys
        ? sql`EXISTS (SELECT 1 FROM ${passkey} WHERE ${passkey.userId} = ${userId} AND (${passkeyId === null} OR ${passkey.id} <> ${passkeyId}))`
        : sql`false`;
      const authenticator = totp && passkeyId !== null
        ? sql`EXISTS (SELECT 1 FROM ${twoFactor} WHERE ${twoFactor.userId} = ${userId} AND ${twoFactor.verified} = true)`
        : sql`false`;
      const guard = sql`(${!required} OR ${otherPasskey} OR ${authenticator})`;
      const matching = passkeyId === null
        ? sql`EXISTS (SELECT 1 FROM ${twoFactor} WHERE ${twoFactor.userId} = ${userId})`
        : sql`EXISTS (SELECT 1 FROM ${passkey} WHERE ${passkey.userId} = ${userId} AND ${passkey.id} = ${passkeyId})`;
      const methods = passkeyId === null ? ['totp', 'backup_code'] : ['passkey'];
      const clearProof = (tx: any, table: typeof session | typeof oauthAccessToken | typeof oauthRefreshToken) =>
        tx.update(table).set({ mfaMethod: null, mfaVerifiedAt: null })
          .where(and(eq(table.userId, userId), inArray(table.mfaMethod, methods), guard, matching));
      const result = await apply((tx) => [
        auditSelected(tx, userId, passkeyId === null ? 'mfa.totp_removed' : 'mfa.passkey_removed', 'removed',
          sql`(${guard} AND ${matching})`),
        // Advance the fence before deleting the factor. Surviving credentials
        // can start a new ceremony at this generation.
        tx.update(user).set({ mfaGeneration: sql`${user.mfaGeneration} + 1` })
          .where(and(eq(user.id, userId), guard, matching)),
        tx.update(session).set({ mfaGeneration: sql`(SELECT ${user.mfaGeneration} FROM ${user} WHERE ${user.id} = ${userId})` })
          .where(and(eq(session.userId, userId), guard, matching)),
        tx.update(passkey).set({ mfaGeneration: sql`(SELECT ${user.mfaGeneration} FROM ${user} WHERE ${user.id} = ${userId})` })
          .where(and(eq(passkey.userId, userId), guard, matching)),
        clearProof(tx, session),
        clearProof(tx, oauthAccessToken),
        clearProof(tx, oauthRefreshToken),
        passkeyId === null
          ? tx.delete(twoFactor).where(and(eq(twoFactor.userId, userId), guard)).returning({ id: twoFactor.id })
          : tx.delete(passkey).where(and(eq(passkey.userId, userId), eq(passkey.id, passkeyId), guard)).returning({ id: passkey.id }),
        ...(passkeyId === null ? [
          tx.update(user).set({ twoFactorEnabled: false }).where(and(eq(user.id, userId),
            sql`NOT EXISTS (SELECT 1 FROM ${twoFactor} WHERE ${twoFactor.userId} = ${userId})`)),
        ] : []),
      ], userId);
      return result[7].length !== 0;
    },
    async claim(key, nowMs, gapMs, max, windowMs = LIMITS.windowMs, lockoutMs = 0) {
      if (key.startsWith('mfa-used:')) {
        await db.delete(t).where(and(sql`${t.key} LIKE 'mfa-used:%'`, lt(t.lastSent, nowMs - windowMs)));
      }
      if (key.startsWith('mfa-attempt:') || key.startsWith('mfa-setup-attempt:') || key.startsWith('mfa-account-attempt:')) {
        await db.delete(t).where(and(
          sql`(${t.key} LIKE 'mfa-attempt:%' OR ${t.key} LIKE 'mfa-setup-attempt:%' OR ${t.key} LIKE 'mfa-account-attempt:%')`,
          lt(t.lastSent, nowMs - lockoutMs),
        ));
      }
      const windowOpen = sql`${t.windowStart} <= ${nowMs - windowMs}`;
      const mayReset = sql`${windowOpen} AND (${t.count} < ${max} OR ${t.lastSent} <= ${nowMs - lockoutMs})`;
      const rows: unknown[] = await db
        .insert(t)
        .values({ key, windowStart: nowMs, count: 1, lastSent: nowMs })
        .onConflictDoUpdate({
          target: t.key,
          set: {
            count: sql`CASE WHEN ${mayReset} THEN 1 ELSE ${t.count} + 1 END`,
            windowStart: sql`CASE WHEN ${mayReset} THEN ${nowMs} ELSE ${t.windowStart} END`,
            lastSent: nowMs,
          },
          setWhere: sql`${t.lastSent} <= ${nowMs - gapMs} AND (${mayReset} OR ${t.count} < ${max})`,
        })
        .returning({ count: t.count });
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
