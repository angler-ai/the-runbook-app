import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { sessionStore, tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { SignInForm } from './SignInForm';
import { NARROW_TOKEN_SUGGESTION, type MeView } from './session';

function me(scopes: string[] = ['runbooks:read']): MeView {
  return {
    user: { id: 'usr_1', name: 'Alice' },
    org: { id: 'org_1', title: 'Acme', role: 'owner' },
    teams: [],
    credential: {
      kind: 'personal',
      actor_kind: 'human',
      scopes,
      projects: null,
      expires_at_ms: Date.now() + 3_600_000,
    },
  };
}

afterEach(() => {
  tokenStore.clear();
  sessionStore.clearCookieSession();
});

describe('SignInForm', () => {
  it('signs in with a pasted token and reports the identity', async () => {
    server.use(http.get('*/v1/me', () => HttpResponse.json(me())));
    const onSignedIn = vi.fn();
    const user = userEvent.setup();
    const { container } = render(<SignInForm secureContext onSignedIn={onSignedIn} />);
    await user.type(screen.getByLabelText(/personal access token/i), 'trb_pat_good');
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    expect(onSignedIn).toHaveBeenCalledWith(
      expect.objectContaining({ user: { id: 'usr_1', name: 'Alice' } }),
    );
    expect(tokenStore.get()).toBe('trb_pat_good');
    await expectNoAxeViolations(container);
  });

  it('explains a too-wide refusal and keeps the token out of storage', async () => {
    server.use(http.get('*/v1/me', () => HttpResponse.json(me(['tokens:manage']))));
    const user = userEvent.setup();
    render(<SignInForm secureContext onSignedIn={vi.fn()} />);
    await user.type(screen.getByLabelText(/personal access token/i), 'trb_pat_wide');
    await user.click(screen.getByRole('button', { name: /sign in/i }));
    expect(await screen.findByText(/too powerful to keep in a browser/i)).toBeInTheDocument();
    expect(within(screen.getByRole('alert')).getByText(/runbook token create/)).toBeInTheDocument();
    expect(tokenStore.get()).toBeNull();
  });

  it('disables pasting outside a secure context (F96)', async () => {
    render(<SignInForm secureContext={false} onSignedIn={vi.fn()} />);
    expect(screen.getByLabelText(/personal access token/i)).toBeDisabled();
    expect(screen.getByText(/not on https or localhost/i)).toBeInTheDocument();
  });

  it('suggests the narrow CLI token and its optional scopes on the page (U3)', () => {
    render(<SignInForm secureContext onSignedIn={vi.fn()} />);
    const field = screen.getByLabelText(/personal access token/i);
    expect(field).toHaveAccessibleDescription(/--scope executions:act/);
    expect(field).toHaveAccessibleDescription(/executions:admin to pause, resume or cancel/);
    expect(field).toHaveAccessibleDescription(/runbooks:write --scope runbooks:publish/);
  });

  it('masks the token without a password field browsers offer to save (U13)', () => {
    render(<SignInForm secureContext onSignedIn={vi.fn()} />);
    const field = screen.getByRole('textbox', { name: /personal access token/i });
    expect(field).toHaveAttribute('type', 'text');
    expect(field).toHaveAttribute('autocomplete', 'off');
    expect(field).toHaveAttribute('spellcheck', 'false');
    expect(field).toHaveAttribute('autocapitalize', 'off');
    expect(field).toHaveAttribute('data-1p-ignore');
    expect(field).toHaveAttribute('data-lpignore', 'true');
    expect(field.className).toContain('[-webkit-text-security:disc]');
    expect(document.querySelector('input[type="password"]')).toBeNull();
  });

  it('shows why the user was sent to sign-in', async () => {
    render(<SignInForm secureContext reason="session-expired" onSignedIn={vi.fn()} />);
    expect(screen.getByText(/session ended/i)).toBeInTheDocument();
  });
});

describe('email code sign-in flow', () => {
  it('sends an email code and verifies it, establishing a cookie session', async () => {
    let sendCalled = false;
    let verifyCalled = false;
    server.use(
      http.post('*/api/auth/email-otp/send-verification-otp', async ({ request }) => {
        sendCalled = true;
        const body = (await request.json()) as { email: string; type: string };
        expect(body.email).toBe('alice@example.test');
        expect(body.type).toBe('sign-in');
        return HttpResponse.json({ status: true });
      }),
      http.post('*/api/auth/sign-in/email-otp', async ({ request }) => {
        verifyCalled = true;
        const body = (await request.json()) as { email: string; otp: string };
        expect(body.email).toBe('alice@example.test');
        expect(body.otp).toBe('123456');
        return HttpResponse.json({ user: { id: 'usr_1', email: 'alice@example.test' } });
      }),
      http.get('*/v1/me', () => HttpResponse.json(me(['tokens:manage']))),
    );

    const onSignedIn = vi.fn();
    const user = userEvent.setup();
    const { container } = render(
      <SignInForm secureContext methods={['email']} onSignedIn={onSignedIn} />,
    );

    // Step 1: email entry
    const emailField = screen.getByLabelText(/email address/i);
    await user.type(emailField, 'alice@example.test');
    await user.click(screen.getByRole('button', { name: /continue with email/i }));

    expect(sendCalled).toBe(true);
    expect(await screen.findByText(/we sent a 6-digit code to/i)).toBeInTheDocument();

    // Step 2: 6-digit code entry
    const otpField = screen.getByLabelText(/6-digit code/i);
    await user.type(otpField, '123456');
    await user.click(screen.getByRole('button', { name: /verify code/i }));

    expect(verifyCalled).toBe(true);
    await waitFor(() => expect(onSignedIn).toHaveBeenCalled());
    expect(sessionStore.isCookieSession()).toBe(true);
    expect(tokenStore.get()).toBeNull();
    await expectNoAxeViolations(container);
  });

  it('displays auth service refusal for uninvited domain', async () => {
    server.use(
      http.post('*/api/auth/email-otp/send-verification-otp', () =>
        HttpResponse.json(
          { code: 'auth.domain_not_allowed', message: 'This email domain cannot sign in here.' },
          { status: 403 },
        ),
      ),
    );
    const user = userEvent.setup();
    render(<SignInForm secureContext methods={['email']} onSignedIn={vi.fn()} />);

    await user.type(screen.getByLabelText(/email address/i), 'stranger@evil.test');
    await user.click(screen.getByRole('button', { name: /continue with email/i }));

    expect(await screen.findByText(/this email domain cannot sign in here/i)).toBeInTheDocument();
  });

  it('displays rate limiting refusal message', async () => {
    server.use(
      http.post('*/api/auth/email-otp/send-verification-otp', () =>
        HttpResponse.json(
          { code: 'auth.rate_limited', message: 'Please wait a minute before requesting another code.' },
          { status: 429 },
        ),
      ),
    );
    const user = userEvent.setup();
    render(<SignInForm secureContext methods={['email']} onSignedIn={vi.fn()} />);

    await user.type(screen.getByLabelText(/email address/i), 'spam@example.test');
    await user.click(screen.getByRole('button', { name: /continue with email/i }));

    expect(
      await screen.findByText(/please wait a minute before requesting another code/i),
    ).toBeInTheDocument();
  });

  it('displays code refusal when code is invalid', async () => {
    server.use(
      http.post('*/api/auth/email-otp/send-verification-otp', () =>
        HttpResponse.json({ status: true }),
      ),
      http.post('*/api/auth/sign-in/email-otp', () =>
        HttpResponse.json(
          { code: 'auth.invalid_code', message: 'Enter the 6-digit code from the email.' },
          { status: 400 },
        ),
      ),
    );
    const user = userEvent.setup();
    render(<SignInForm secureContext methods={['email']} onSignedIn={vi.fn()} />);

    await user.type(screen.getByLabelText(/email address/i), 'alice@example.test');
    await user.click(screen.getByRole('button', { name: /continue with email/i }));

    const otpField = await screen.findByLabelText(/6-digit code/i);
    await user.type(otpField, '999999');
    await user.click(screen.getByRole('button', { name: /verify code/i }));

    expect(
      await screen.findByText(/enter the 6-digit code from the email/i),
    ).toBeInTheDocument();
  });
});

describe('Google sign-in and OAuth error display', () => {
  it('renders Google sign-in button when google method is enabled', async () => {
    let socialCalled = false;
    server.use(
      http.post('*/api/auth/sign-in/social', async ({ request }) => {
        socialCalled = true;
        const body = (await request.json()) as { provider: string };
        expect(body.provider).toBe('google');
        return HttpResponse.json({ url: 'https://accounts.google.com/o/oauth2/auth?...' });
      }),
    );
    const user = userEvent.setup();
    const { container } = render(
      <SignInForm secureContext methods={['google']} onSignedIn={vi.fn()} />,
    );

    const googleBtn = screen.getByRole('button', { name: /continue with google/i });
    expect(googleBtn).toBeInTheDocument();
    await user.click(googleBtn);
    expect(socialCalled).toBe(true);
    await expectNoAxeViolations(container);
  });

  it('displays OAuth error message from query param', () => {
    render(
      <SignInForm
        secureContext
        oauthError="access_denied"
        methods={['google']}
        onSignedIn={vi.fn()}
      />,
    );
    expect(screen.getByText(/sign-in error: access_denied/i)).toBeInTheDocument();
  });
});

describe('the CLI command in the sign-in help (walkthrough #13)', () => {
  it('is a code block that wraps at spaces only, with a copy button', async () => {
    const user = userEvent.setup();
    render(<SignInForm secureContext onSignedIn={() => {}} />);
    const block = screen.getByTestId('token-command');
    const code = block.querySelector('pre > code') as HTMLElement;
    expect(within(code).getByText(NARROW_TOKEN_SUGGESTION)).toHaveClass('sr-only');
    expect(code.querySelector('[data-slot="command-words"]')?.textContent).toBe(
      NARROW_TOKEN_SUGGESTION,
    );
    // Every word is unbreakable: a line never ends after "--" or inside a word.
    const words = Array.from(code.querySelectorAll('span.whitespace-nowrap'), (w) => w.textContent);
    expect(words).toEqual(NARROW_TOKEN_SUGGESTION.split(' '));
    // Wrap between words, never inside one ("runbo / ok").
    expect(code.className).toMatch(/whitespace-pre-wrap/);
    expect(code.className).toMatch(/break-normal/);
    expect(code.className).not.toMatch(/break-all/);
    await user.click(within(block).getByRole('button', { name: 'Copy command' }));
    expect(await navigator.clipboard.readText()).toBe(NARROW_TOKEN_SUGGESTION);
    expect(within(block).getByRole('button', { name: 'Copied' })).toBeInTheDocument();
  });
});

describe('the access token option', () => {
  it('is folded under a developers option when email sign-in exists', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <SignInForm secureContext methods={['email']} onSignedIn={vi.fn()} />,
    );
    const details = container.querySelector('details');
    expect(details?.open).toBe(false);
    await user.click(screen.getByText('Sign in with an access token (developers)'));
    expect(details?.open).toBe(true);
    expect(screen.getByLabelText('Personal access token')).toBeInTheDocument();
  });

  it('is the form itself when no other method is configured', () => {
    const { container } = render(<SignInForm secureContext methods={[]} onSignedIn={vi.fn()} />);
    expect(container.querySelector('details')).toBeNull();
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeInTheDocument();
  });
});
