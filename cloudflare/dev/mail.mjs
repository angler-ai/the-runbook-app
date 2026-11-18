// CF7: the local mail stub (wrangler.mail.jsonc). LOCAL ONLY: nothing is
// sent; codes are logged and kept in memory.
import { WorkerEntrypoint } from 'cloudflare:workers';

const messages = [];

export class Mail extends WorkerEntrypoint {
  async send(message) {
    const code = /^(\d{6}) /.exec(message.subject ?? '')?.[1] ?? null;
    messages.push({ to: message.to, code });
    if (messages.length > 50) messages.shift();
    console.log(`[mail] sign-in code for ${message.to}: ${code ?? '(none)'}`);
    return { messageId: `local-${messages.length}` };
  }
}

export default {
  fetch() {
    return Response.json(messages);
  },
};
