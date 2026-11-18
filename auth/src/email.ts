// Delivery of sign-in codes. The transports take their I/O as arguments, so this
// module has no Node or Workers imports and both bundles can include it:
// server.ts passes nodemailer and node:fs, worker.ts passes its EMAIL binding.
// Never log a code, an address or a provider error (they can contain both).

export interface CodeMessage {
  from: string;
  to: string;
  subject: string;
  text: string;
  html: string;
}

export type MailKind = 'sign-in' | 'notification';

export interface MailTransport {
  /** Rejects when the message was not accepted; the caller then voids the code. */
  send(message: CodeMessage, otp: string, kind?: MailKind): Promise<void>;
}

const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

export function codeMessage(from: string, to: string, otp: string): CodeMessage {
  return {
    from,
    to,
    subject: `${otp} is your The Runbook sign-in code`,
    text: `Your The Runbook sign-in code is ${otp}. It expires in 10 minutes and works once.\n\nIf you did not request this code, you can ignore this email.`,
    html: `<div style="font-family:system-ui,sans-serif;max-width:440px;margin:auto;padding:24px"><p>Your The Runbook sign-in code:</p><p style="font-size:32px;letter-spacing:8px;font-weight:700">${escapeHtml(otp)}</p><p style="color:#555">It expires in 10 minutes and works once. If you did not request this code, you can ignore this email.</p></div>`,
  };
}

/** Cloudflare's `send_email` binding (the shape Shelf uses), or the test mock of it. */
export interface EmailBinding {
  send(message: { from: string; to: string; subject: string; text: string; html: string }): Promise<unknown>;
}

export function bindingTransport(binding: EmailBinding): MailTransport {
  return {
    async send(message) {
      await binding.send(message);
    },
  };
}

/** Cloudflare Email Service's `send_email` binding: Shelf's call, a structured From. */
export interface EmailServiceBinding {
  send(message: { from: { name?: string; email: string }; to: string; subject: string; text: string; html: string }): Promise<unknown>;
}

/**
 * Cloudflare Email Service (RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare): the code
 * goes out through the EMAIL binding as Shelf sends it. A rejection voids the
 * code (the caller); nothing here logs the message or the provider's error.
 */
export function cloudflareTransport(binding: EmailServiceBinding, sender: { name?: string; email: string }): MailTransport {
  return {
    async send(message) {
      await binding.send({ from: sender, to: message.to, subject: message.subject, text: message.text, html: message.html });
    },
  };
}

/** The subset of nodemailer's `createTransport` this module needs. */
export type CreateSmtpTransport = (url: string) => { sendMail(message: CodeMessage): Promise<unknown> };

export function smtpTransport(createTransport: CreateSmtpTransport, smtpUrl: string): MailTransport {
  const mailer = createTransport(smtpUrl);
  return {
    async send(message) {
      await mailer.sendMail(message);
    },
  };
}

/**
 * Test-only (D48): appends one JSON line per code to a file, for e2e tests and
 * local development. config.ts refuses it unless RUNBOOK_ORIGIN is loopback.
 * Line format: {"at": ISO-8601, "to": email, "type": "sign-in", "code": "123456", "subject": string}
 */
export function captureTransport(file: string, appendLine: (file: string, line: string) => Promise<void>): MailTransport {
  return {
    async send(message, otp, kind = 'sign-in') {
      const line = JSON.stringify({ at: new Date().toISOString(), to: message.to, type: kind, code: otp, subject: message.subject });
      await appendLine(file, `${line}\n`);
    },
  };
}
