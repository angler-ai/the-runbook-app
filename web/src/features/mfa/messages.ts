// What people read for the sign-in and second-factor error codes
// (auth/README.md "Error codes" and "Multi-factor authentication"), from the
// auth service (`/api/auth`) or the core API. One table, so a code reads the
// same wherever it surfaces.

/** Where an operator recovers an account an owner cannot reset. */
export const OPERATOR_RESET_HINT =
  'The operator of this installation can: “runbook-server admin mfa reset <email>” on a self-hosted server, or “npm run mfa:reset -- <email> --operator <name>” in the cloudflare folder on Cloudflare.';

const SECURITY_MESSAGES: Record<string, string> = {
  'auth.mfa_step_up': 'Confirm it is you with a second factor, then try again.',
  'auth.mfa_required':
    'Your organization requires a second factor. Confirm it is you with one, then try again.',
  'auth.mfa_locked': 'Too many attempts. Wait 15 minutes, then try again.',
  'auth.mfa_code_reused': 'That code was just used. Wait for your authenticator to show a new one.',
  'auth.mfa_factor_disallowed':
    'Your organization does not allow this kind of factor. Use one it allows, or ask an owner which factors count.',
  'auth.mfa_setup_changed':
    'This setup ended or was replaced (setups last 10 minutes and belong to the browser that started them). Start again.',
  'auth.mfa_already_enrolled':
    'An authenticator app is already set up for your account. Remove it first to set up a new one.',
  'auth.mfa_removal_refused':
    'Your organization requires a second factor, so you cannot remove your last allowed one. Add another factor first.',
  'auth.sign_in_again':
    'Adding a passkey needs a sign-in from the last 24 hours. Sign out, sign in again, then add the passkey.',
  'auth.passkey_assertion_failed':
    'That passkey could not be verified. Try again, or use another passkey for this account.',
  'auth.passkey_uv_required':
    'Your passkey must check it is you (a PIN, fingerprint or face). Try again and confirm on the device.',
  'auth.passkey_account_mismatch':
    'That passkey belongs to a different account. Use a passkey for the account you are signed in to.',
  'auth.mfa_token_replacement_required':
    'This token was created without a fresh second-factor check, and your organization requires one for this action. Create a replacement in Account settings › Personal tokens (or run “runbook login” again), then use the new token.',
  'auth.mfa_operator_reset_required': `This member also belongs to another organization or shares a sign-in, so an owner cannot reset them. ${OPERATOR_RESET_HINT}`,
  'auth.unavailable': 'The sign-in service is not answering. Try again in a minute.',
  'auth.no_session': 'Your session ended. Sign in again.',
  'auth.rate_limited': 'Too many attempts. Try again later.',
  'auth.csrf': 'This page is out of date. Reload it and try again.',
  'auth.invalid_request': 'The sign-in service could not read that request. Reload and try again.',
  'auth.not_configured': 'Sign-in is not set up on this server.',
  'auth.misconfigured': 'Sign-in is misconfigured on this server. Ask its operator to check it.',
};

/** The message for a security-related code, or null for other codes. */
export function securityCodeMessage(code: string): string | null {
  return SECURITY_MESSAGES[code] ?? null;
}

/** Every code with a message here (tests check auth/README.md against it). */
export const SECURITY_CODES: readonly string[] = Object.keys(SECURITY_MESSAGES);
