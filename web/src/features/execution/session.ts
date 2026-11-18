// Per-tab actor session and claim bookkeeping (M2). The session_id is
// stable for the tab (sessionStorage), and claims this tab holds are
// remembered the same way, so a reload keeps working on its own claim
// instead of reporting it as claimed elsewhere (F100 distinguishes the
// two by whether the claim ref is in this store). The store is only a
// hint: the page reconciles it with every document it reads (U5), using
// when each claim was taken to tell a fresh document from a stale one.
import type { Schemas } from '../../api/client';
import { onSessionEnd } from '../../api/session-end';

export type ClaimRef = Schemas['ClaimRef'];

const SESSION_KEY = 'runbook.tab-session.v1';
const CLAIMS_KEY = 'runbook.tab-claims.v1';

function read(key: string): string | null {
  try {
    return globalThis.sessionStorage?.getItem(key) ?? null;
  } catch {
    return null;
  }
}
function write(key: string, value: string) {
  try {
    globalThis.sessionStorage?.setItem(key, value);
  } catch {
    // Memory copy below still applies for this page load.
  }
}
function remove(key: string) {
  try {
    globalThis.sessionStorage?.removeItem(key);
  } catch {
    // Ignore: stale values are revalidated by the server on use.
  }
}

let memorySessionId: string | null = null;

export function tabSessionId(): string {
  memorySessionId ??= read(SESSION_KEY);
  if (!memorySessionId) {
    memorySessionId = `web-${globalThis.crypto.randomUUID()}`;
    write(SESSION_KEY, memorySessionId);
  }
  return memorySessionId;
}

interface HeldClaim {
  claim: ClaimRef;
  /** Epoch ms when this tab took the claim; 0 when unknown (an older record). */
  at: number;
}

let memoryClaims: Record<string, HeldClaim> | null = null;

function isClaimRef(value: unknown): value is ClaimRef {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as ClaimRef).id === 'string' &&
    typeof (value as ClaimRef).generation === 'number'
  );
}

function loadClaims(): Record<string, HeldClaim> {
  if (memoryClaims) return memoryClaims;
  memoryClaims = {};
  try {
    const raw = read(CLAIMS_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    for (const [stepRunId, value] of Object.entries(parsed as Record<string, unknown>)) {
      // Current records are { claim, at }; earlier ones were the bare ClaimRef.
      const held = value as Partial<HeldClaim>;
      if (isClaimRef(held.claim)) {
        memoryClaims[stepRunId] = { claim: held.claim, at: Number(held.at) || 0 };
      } else if (isClaimRef(value)) {
        memoryClaims[stepRunId] = { claim: value, at: 0 };
      }
    }
  } catch {
    memoryClaims = {};
  }
  return memoryClaims;
}

function persistClaims() {
  if (!memoryClaims) return;
  if (Object.keys(memoryClaims).length === 0) remove(CLAIMS_KEY);
  else write(CLAIMS_KEY, JSON.stringify(memoryClaims));
}

export const tabClaims = {
  get(stepRunId: string): ClaimRef | undefined {
    return loadClaims()[stepRunId]?.claim;
  },
  /** When this tab took the claim (epoch ms; 0 if unknown). */
  takenAt(stepRunId: string): number | undefined {
    return loadClaims()[stepRunId]?.at;
  },
  set(stepRunId: string, claim: ClaimRef, at: number = Date.now()) {
    loadClaims()[stepRunId] = { claim: { id: claim.id, generation: claim.generation }, at };
    persistClaims();
  },
  clear(stepRunId: string) {
    delete loadClaims()[stepRunId];
    persistClaims();
  },
  all(): Record<string, ClaimRef> {
    return Object.fromEntries(
      Object.entries(loadClaims()).map(([stepRunId, held]) => [stepRunId, held.claim]),
    );
  },
};

/**
 * Forget this tab's actor session and claims (U1): the next person to sign
 * in here gets a session id of their own and none of the last one's
 * claims. Registered with endSession, which has already removed the
 * storage keys; the memory copies would otherwise outlive the session.
 */
export function resetTabSession() {
  memorySessionId = null;
  memoryClaims = null;
  remove(SESSION_KEY);
  remove(CLAIMS_KEY);
}

onSessionEnd(resetTabSession);

/**
 * Whether the document still shows `claim` as held. `documentAt` is when
 * the document was read; a document read before the claim was taken cannot
 * know about it, so a step it shows as merely ready proves nothing.
 */
export function claimStillHeld(
  claim: ClaimRef,
  takenAt: number,
  section: { status: string },
  step: { claim?: ClaimRef | null } | undefined,
  documentAt: number,
): boolean {
  const shown = step?.claim;
  if (
    section.status === 'claimed' &&
    shown?.id === claim.id &&
    shown.generation === claim.generation
  ) {
    return true;
  }
  // Read after the claim was taken and it disagrees: the claim is gone.
  if (documentAt > takenAt) return false;
  // An older read: drop only on proof of something newer than the claim,
  // a later claim or a finished step.
  if (shown && shown.generation >= claim.generation) return false;
  return ['pending', 'ready', 'claimed'].includes(section.status);
}
