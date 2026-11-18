// The pasted PAT lives in memory plus sessionStorage (tab-scoped, A3).
// Every storage access is wrapped in try/catch: privacy modes and embedded
// webviews throw on access, and sign-in must still work in memory there.
// The token is never written to localStorage, cookies, logs, or URLs.

const STORAGE_KEY = 'runbook.session.token.v1';

type Listener = (token: string | null) => void;

function readStored(): string | null {
  try {
    const value = globalThis.sessionStorage?.getItem(STORAGE_KEY);
    return typeof value === 'string' && value.length > 0 ? value : null;
  } catch {
    return null;
  }
}

function writeStored(token: string): boolean {
  try {
    globalThis.sessionStorage?.setItem(STORAGE_KEY, token);
    return readStored() === token;
  } catch {
    return false;
  }
}

function removeStored() {
  try {
    globalThis.sessionStorage?.removeItem(STORAGE_KEY);
  } catch {
    // In-memory clear still applies; a stuck stored value is revalidated
    // against /v1/me on the next load and dropped if the server rejects it.
  }
}

export interface TokenStore {
  get(): string | null;
  /** Keeps the token in memory; returns whether sessionStorage also holds it. */
  set(token: string): boolean;
  clear(): void;
  subscribe(listener: Listener): () => void;
}

export type SessionKind = 'pat' | 'cookie' | null;

const SESSION_KIND_KEY = 'runbook.session.kind.v1';

// A pasted token is tab-scoped (sessionStorage, A3). A cookie session is
// the browser's own 30-day cookie, so its marker is in localStorage: a new
// tab, or a phone browser that dropped the tab, stays signed in. The marker
// is only a flag; the cookie is the credential.
function readStoredKind(): SessionKind {
  try {
    if (globalThis.sessionStorage?.getItem(SESSION_KIND_KEY) === 'pat') return 'pat';
  } catch {
    // sessionStorage unavailable
  }
  try {
    if (globalThis.localStorage?.getItem(SESSION_KIND_KEY) === 'cookie') return 'cookie';
  } catch {
    // localStorage unavailable
  }
  return null;
}

function writeStoredKind(kind: SessionKind) {
  try {
    if (kind === 'pat') globalThis.sessionStorage?.setItem(SESSION_KIND_KEY, kind);
    else globalThis.sessionStorage?.removeItem(SESSION_KIND_KEY);
  } catch {
    // sessionStorage unavailable
  }
  try {
    if (kind === 'cookie') globalThis.localStorage?.setItem(SESSION_KIND_KEY, kind);
    else globalThis.localStorage?.removeItem(SESSION_KIND_KEY);
  } catch {
    // localStorage unavailable
  }
}

export interface SessionStore {
  getKind(): SessionKind;
  setCookieSession(): void;
  clearCookieSession(): void;
  isCookieSession(): boolean;
  isPatSession(): boolean;
  subscribe(listener: (kind: SessionKind) => void): () => void;
}

export function createSessionStore(tokenStoreInstance: TokenStore): SessionStore {
  let kind: SessionKind = tokenStoreInstance.get() ? 'pat' : readStoredKind();
  const listeners = new Set<(kind: SessionKind) => void>();
  const notify = () => {
    for (const listener of listeners) listener(kind);
  };

  tokenStoreInstance.subscribe((token) => {
    if (token) {
      kind = 'pat';
      writeStoredKind('pat');
    } else if (kind === 'pat') {
      kind = null;
      writeStoredKind(null);
    }
    notify();
  });

  return {
    getKind: () => (tokenStoreInstance.get() ? 'pat' : kind),
    setCookieSession: () => {
      tokenStoreInstance.clear();
      kind = 'cookie';
      writeStoredKind('cookie');
      notify();
    },
    clearCookieSession: () => {
      if (kind === 'cookie') {
        kind = null;
        writeStoredKind(null);
        notify();
      }
    },
    isCookieSession: () => tokenStoreInstance.get() === null && kind === 'cookie',
    isPatSession: () => tokenStoreInstance.get() !== null,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

export function createTokenStore(): TokenStore {
  let token: string | null = readStored();
  const listeners = new Set<Listener>();
  const notify = () => {
    for (const listener of listeners) listener(token);
  };
  return {
    get: () => token,
    set(next: string) {
      token = next;
      const persisted = writeStored(next);
      notify();
      return persisted;
    },
    clear() {
      token = null;
      removeStored();
      notify();
    },
    subscribe(listener: Listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

/** App-wide token store, restored from sessionStorage at module load. */
export const tokenStore: TokenStore = createTokenStore();
export const sessionStore: SessionStore = createSessionStore(tokenStore);

