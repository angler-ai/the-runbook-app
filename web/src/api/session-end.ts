// One exit for every way a session ends (U1): header or palette sign-out,
// a 401 for the current token, and a restored token that no longer
// qualifies. The next person in this tab must see nothing of the last
// one, so ending a session drops the token, every `runbook.*` key in
// sessionStorage (token, tab claims, tab session id, palette recents, and
// any tab-scoped key added later), and runs the in-memory cleanups that
// features register (the React Query cache, the in-memory tab claims).

import { sessionStore, tokenStore } from './token-store';

const TAB_KEY_PREFIX = 'runbook.';

type Cleanup = () => void;
const cleanups = new Set<Cleanup>();

/** Run `cleanup` whenever a session ends; returns the unregister function. */
export function onSessionEnd(cleanup: Cleanup): () => void {
  cleanups.add(cleanup);
  return () => {
    cleanups.delete(cleanup);
  };
}

function clearTabStorage() {
  try {
    const storage = globalThis.sessionStorage;
    if (!storage) return;
    const keys: string[] = [];
    for (let i = 0; i < storage.length; i += 1) {
      const key = storage.key(i);
      if (key?.startsWith(TAB_KEY_PREFIX)) keys.push(key);
    }
    for (const key of keys) storage.removeItem(key);
  } catch {
    // Storage blocked: the in-memory cleanups below still run.
  }
}

export function endSession(): void {
  tokenStore.clear();
  sessionStore.clearCookieSession();
  clearTabStorage();
  for (const cleanup of [...cleanups]) {
    try {
      cleanup();
    } catch {
      // One failing cleanup must not leave the others undone.
    }
  }
}

