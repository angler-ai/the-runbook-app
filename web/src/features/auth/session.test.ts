import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { registerUnauthorizedHandler } from '../../api/client';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { tabClaims } from '../execution/session';
import {
  NARROW_TOKEN_SUGGESTION,
  attemptSignIn,
  isSecurePasteContext,
  signOut,
  validateMeForSignIn,
  type MeView,
} from './session';

const NOW = 1_800_000_000_000;

function me(
  overrides: Partial<MeView['credential']> = {},
  user: MeView['user'] = { id: 'usr_1', name: 'Alice' },
): MeView {
  return {
    user,
    org: { id: 'org_1', title: 'Acme', role: 'owner' },
    teams: [{ id: 'team_1', slug: 'oncall' }],
    credential: {
      kind: 'personal',
      actor_kind: 'human',
      scopes: ['runbooks:read', 'executions:read'],
      projects: null,
      expires_at_ms: NOW + 60_000,
      ...overrides,
    },
  };
}

function meHandler(view: MeView) {
  return http.get('*/v1/me', () => HttpResponse.json(view));
}

afterEach(() => {
  tokenStore.clear();
});

describe('validateMeForSignIn (F90)', () => {
  it('accepts a narrow human PAT', () => {
    expect(validateMeForSignIn(me(), NOW)).toBeNull();
  });
  it('refuses service tokens', () => {
    expect(validateMeForSignIn(me({ kind: 'service', actor_kind: null }, null), NOW)?.kind).toBe(
      'service',
    );
  });
  it('refuses agent-kind tokens', () => {
    expect(validateMeForSignIn(me({ actor_kind: 'agent' }), NOW)?.kind).toBe('agent');
  });
  it('refuses expired tokens', () => {
    expect(validateMeForSignIn(me({ expires_at_ms: NOW - 1 }), NOW)?.kind).toBe('expired');
  });
  it('refuses every scope outside the browser allow-list, naming it (U3)', () => {
    for (const scope of [
      'tokens:manage',
      'webhooks:manage',
      'project:admin',
      'skills:write',
      'future:thing',
    ]) {
      const refusal = validateMeForSignIn(me({ scopes: ['runbooks:read', scope] }), NOW);
      expect(refusal?.kind).toBe('too-wide');
      expect(refusal?.message).toContain(`That token carries ${scope},`);
      expect(refusal?.message).not.toContain('runbooks:read,');
      expect(refusal?.message).toContain('runbook token create');
    }
  });
  it('accepts every allow-listed scope, executions:admin included (D27)', () => {
    const scopes = [
      'runbooks:read',
      'runbooks:write',
      'runbooks:publish',
      'executions:read',
      'executions:start',
      'executions:act',
      'executions:admin',
      'skills:read',
    ];
    expect(validateMeForSignIn(me({ scopes }), NOW)).toBeNull();
  });
  it('suggests the narrow set and names the optional scopes', () => {
    expect(NARROW_TOKEN_SUGGESTION).toContain(
      '--scope runbooks:read --scope executions:read --scope executions:start --scope executions:act',
    );
    expect(NARROW_TOKEN_SUGGESTION).not.toContain('admin');
    const refusal = validateMeForSignIn(me({ scopes: ['webhooks:manage'] }), NOW);
    expect(refusal?.message).toContain('executions:admin');
    expect(refusal?.message).toContain('runbooks:write');
  });
});

describe('isSecurePasteContext (F96)', () => {
  it('allows https and loopback hosts', () => {
    expect(isSecurePasteContext('https://runbook.example/sign-in')).toBe(true);
    expect(isSecurePasteContext('http://localhost:5173/sign-in')).toBe(true);
    expect(isSecurePasteContext('http://runbook.localhost:8080/sign-in')).toBe(true);
    expect(isSecurePasteContext('http://nested.RUNBOOK.localhost./sign-in')).toBe(true);
    expect(isSecurePasteContext('http://127.0.0.1:8080/')).toBe(true);
    expect(isSecurePasteContext('http://127.1.2.3/')).toBe(true);
    expect(isSecurePasteContext('http://[::1]/')).toBe(true);
    expect(isSecurePasteContext('http://runbook.localhost.example/')).toBe(false);
    expect(isSecurePasteContext('http://notlocalhost/')).toBe(false);
    expect(isSecurePasteContext('http://runbook.lan/sign-in')).toBe(false);
    expect(isSecurePasteContext('http://192.168.1.10/')).toBe(false);
  });
});

describe('attemptSignIn', () => {
  it('stores the token only after a valid /v1/me', async () => {
    server.use(meHandler(me()));
    const result = await attemptSignIn('  trb_pat_good  ', { secureContext: true, now: NOW });
    expect(result).toMatchObject({ ok: true });
    expect(tokenStore.get()).toBe('trb_pat_good');
    expect(sessionStorage.getItem('runbook.session.token.v1')).toBe('trb_pat_good');
  });

  it('refuses each kind without storing anything', async () => {
    const cases: [MeView, string][] = [
      [me({ kind: 'service', actor_kind: null }, null), 'service'],
      [me({ actor_kind: 'agent' }), 'agent'],
      [me({ expires_at_ms: NOW - 5 }), 'expired'],
      [me({ scopes: ['tokens:manage'] }), 'too-wide'],
    ];
    for (const [view, kind] of cases) {
      server.use(meHandler(view));
      const result = await attemptSignIn('trb_pat_x', { secureContext: true, now: NOW });
      expect(result).toMatchObject({ ok: false, refusal: { kind } });
      expect(tokenStore.get()).toBeNull();
      expect(sessionStorage.getItem('runbook.session.token.v1')).toBeNull();
    }
  });

  it('refuses the paste in an insecure context before any request', async () => {
    let called = false;
    server.use(
      http.get('*/v1/me', () => {
        called = true;
        return HttpResponse.json(me());
      }),
    );
    const result = await attemptSignIn('trb_pat_x', { secureContext: false, now: NOW });
    expect(result).toMatchObject({ ok: false, refusal: { kind: 'insecure' } });
    expect(called).toBe(false);
    expect(tokenStore.get()).toBeNull();
  });

  it('refuses a token with inner whitespace or control characters before any request (U15)', async () => {
    let called = false;
    server.use(
      http.get('*/v1/me', () => {
        called = true;
        return HttpResponse.json(me());
      }),
    );
    for (const pasted of [
      'trb_pat_ab\ncd',
      'trb_pat_ab cd',
      'trb_pat_ab\tcd',
      'trb_pat_ab\u0000cd',
      'trb_pat_é',
    ]) {
      const result = await attemptSignIn(pasted, { secureContext: true, now: NOW });
      expect(result).toMatchObject({ ok: false, refusal: { kind: 'invalid' } });
      if (!result.ok) {
        expect(result.refusal.message).toMatch(/space, line break or other character/);
        expect(result.refusal.message).not.toMatch(/could not be reached/);
      }
    }
    expect(called).toBe(false);
    expect(tokenStore.get()).toBeNull();
  });

  it('maps a 401 to an invalid-credential refusal', async () => {
    server.use(
      http.get('*/v1/me', () =>
        HttpResponse.json(
          {
            code: 'auth.invalid_credential',
            status: 401,
            detail: 'x',
            type: 't',
            title: 't',
            instance: 'urn:request:req_1',
            errors: [],
          },
          { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    const result = await attemptSignIn('trb_pat_bad', { secureContext: true, now: NOW });
    expect(result).toMatchObject({ ok: false, refusal: { kind: 'invalid' } });
    expect(tokenStore.get()).toBeNull();
  });
});

describe('signOut (B1.5)', () => {
  it('revokes the presenting token, then clears storage', async () => {
    let revokedWith: string | null = null;
    server.use(
      http.delete('*/v1/me/token', ({ request }) => {
        revokedWith = request.headers.get('authorization');
        return new HttpResponse(null, { status: 204 });
      }),
    );
    tokenStore.set('trb_pat_good');
    const result = await signOut();
    expect(result).toEqual({ revoked: true });
    expect(revokedWith).toBe('Bearer '.concat('trb_pat_good'));
    expect(tokenStore.get()).toBeNull();
    expect(sessionStorage.getItem('runbook.session.token.v1')).toBeNull();
  });

  it('still clears storage when revocation fails', async () => {
    server.use(http.delete('*/v1/me/token', () => HttpResponse.error()));
    tokenStore.set('trb_pat_good');
    const result = await signOut();
    expect(result).toEqual({ revoked: false });
    expect(tokenStore.get()).toBeNull();
  });

  it('ends the session: every runbook.* tab key and the in-memory claims go (U1)', async () => {
    server.use(http.delete('*/v1/me/token', () => new HttpResponse(null, { status: 204 })));
    tokenStore.set('trb_pat_good');
    tabClaims.set('sr_1', { id: 'clm_1', generation: 1 });
    sessionStorage.setItem('runbook.palette.recents.v1', '[]');
    sessionStorage.setItem('unrelated', 'kept');
    await signOut();
    expect(tabClaims.all()).toEqual({});
    expect(Object.keys(sessionStorage).filter((k) => k.startsWith('runbook.'))).toEqual([]);
    expect(sessionStorage.getItem('unrelated')).toBe('kept');
    sessionStorage.removeItem('unrelated');
  });

  it('treats a 401 from an already-revoked token as revoked, without a session-expired redirect', async () => {
    server.use(
      http.delete('*/v1/me/token', () =>
        HttpResponse.json(
          { code: 'auth.invalid_credential', status: 401 },
          { status: 401, headers: { 'Content-Type': 'application/problem+json' } },
        ),
      ),
    );
    const handler = vi.fn();
    registerUnauthorizedHandler(handler);
    tokenStore.set('trb_pat_gone');
    expect(await signOut()).toEqual({ revoked: true });
    expect(handler).not.toHaveBeenCalled();
    registerUnauthorizedHandler(null);
  });
});

describe('attemptSignIn over a live session (U1)', () => {
  it('ends the previous session before storing the new token', async () => {
    server.use(meHandler(me()));
    tokenStore.set('trb_pat_previous');
    tabClaims.set('sr_prev', { id: 'clm_prev', generation: 1 });
    const result = await attemptSignIn('trb_pat_next', { secureContext: true, now: NOW });
    expect(result).toMatchObject({ ok: true });
    expect(tokenStore.get()).toBe('trb_pat_next');
    expect(tabClaims.all()).toEqual({});
  });
});
