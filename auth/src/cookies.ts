// Gate fix 7: the session cookie names, and the one-release migration from the
// pre-__Host- name. Shared by the gateway (app.ts) and /internal/session.
/**
 * Gate fix 7: on https the session cookie is `__Host-runbook.session_token`
 * (Secure, Path=/, no Domain: only this exact origin can set or read it). Before,
 * it was `__Secure-runbook.session_token`; that name is still accepted for one
 * release (read as the new one, expired when the new one is set).
 */
export const SESSION_COOKIE = { https: '__Host-runbook.session_token', http: 'runbook.session_token', legacy: '__Secure-runbook.session_token' } as const;

/**
 * The Cookie header with a legacy session cookie also sent under the current
 * name (when only the legacy one is present), or null when nothing changes.
 */
export function migrateCookieHeader(header: string | null | undefined, secure: boolean): string | null {
  if (!secure || !header) return null;
  const pairs = header.split(';').map((p) => p.trim()).filter(Boolean);
  const value = (name: string) => pairs.find((p) => p.startsWith(`${name}=`))?.slice(name.length + 1);
  const legacy = value(SESSION_COOKIE.legacy);
  if (!legacy || value(SESSION_COOKIE.https) !== undefined) return null;
  return `${header}; ${SESSION_COOKIE.https}=${legacy}`;
}
