// Fails fast when Playwright is started without the harness.
import { loadState } from './support/state.ts';

export default async function globalSetup() {
  // UI-4: a spec run against a review stack (scripts/dev/review.sh) has no
  // harness state; it names the stack itself (see builder.spec.ts).
  if (process.env.E2E_REVIEW_STATE_DIR) return;
  const state = loadState();
  const response = await fetch(`${state.baseURL}/v1/meta`).catch(() => null);
  if (!response?.ok) {
    throw new Error(
      `The e2e stack at ${state.baseURL} is not answering. Start it with \`npm run e2e\` (or \`npm run e2e -- --serve\`).`,
    );
  }
}
