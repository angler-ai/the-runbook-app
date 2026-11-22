// `/settings/security?return_to=<encoded local path>` resumes an OAuth (MCP
// client) authorization that the organization's MFA policy interrupted. Only
// a path on this origin is honoured, so the parameter cannot send anyone to
// another site.

export function safeReturnTo(
  value: unknown,
  origin: string,
): string | null {
  if (typeof value !== 'string' || value.length === 0 || value.length > 4096) return null;
  if (!value.startsWith('/') || value.startsWith('//') || value.includes('\\')) return null;
  // Control characters (including tab and newline, which URL parsing strips) never belong here.
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) return null;
  }
  try {
    const url = new URL(value, origin);
    if (url.origin !== new URL(origin).origin) return null;
    const path = value.split(/[?#]/, 1)[0];
    if (path !== url.pathname) return null;
    if (path !== '/api/auth/oauth2/authorize' && path !== '/api/auth/oauth/consent') return null;
    return `${url.pathname}${url.search}${url.hash}`;
  } catch {
    return null;
  }
}
