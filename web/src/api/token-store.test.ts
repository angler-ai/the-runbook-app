import { afterEach, describe, expect, it } from 'vitest';
import { createSessionStore, createTokenStore } from './token-store';

const KEY = 'runbook.session.kind.v1';

afterEach(() => {
  sessionStorage.clear();
  localStorage.clear();
});

describe('session store', () => {
  it('keeps a cookie session across tabs (localStorage), not per tab', () => {
    createSessionStore(createTokenStore()).setCookieSession();
    expect(localStorage.getItem(KEY)).toBe('cookie');
    // A new tab: its sessionStorage is empty, the marker is still there.
    sessionStorage.clear();
    expect(createSessionStore(createTokenStore()).getKind()).toBe('cookie');
  });

  it('keeps a pasted token tab-scoped', () => {
    const tokens = createTokenStore();
    createSessionStore(tokens);
    tokens.set('trb_pat_example');
    expect(sessionStorage.getItem(KEY)).toBe('pat');
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  it('signing out of a cookie session clears the marker everywhere', () => {
    const store = createSessionStore(createTokenStore());
    store.setCookieSession();
    store.clearCookieSession();
    expect(localStorage.getItem(KEY)).toBeNull();
    expect(createSessionStore(createTokenStore()).getKind()).toBeNull();
  });
});
