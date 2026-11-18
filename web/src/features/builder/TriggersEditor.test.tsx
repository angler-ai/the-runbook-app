// The builder's Triggers section: adding schedules and webhooks writes
// `spec.triggers`, the form's choices become cron expressions, inputs take
// their declared types, the compiler's problems show under the field, and
// removing the last trigger leaves no empty `triggers:` behind.
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parseDocument } from 'yaml';
import { tokenStore } from '../../api/token-store';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import type { Diagnostic } from '../../wasm/runbook';
import { rememberEditorTab } from '../runbook/tabs';
import { editorHandlers, fakeCompiler, renderEditor } from '../editor/test-support';

vi.mock('../editor/YamlEditor', async () => ({
  default: (await import('../editor/test-support')).StubYamlEditor,
}));

const SOURCE = `metadata:
  id: post-deploy
  title: Post-deploy verification
spec:
  inputs:
    service: { type: string, required: true }
    replicas: { type: integer, default: 2 }
    region: { type: enum, options: [us, eu] }
  steps:
    - id: health
      title: Health answers 200
`;

const yamlText = () =>
  (screen.getByRole('textbox', { name: /Runbook YAML/ }) as HTMLTextAreaElement).value;
type Triggers = { schedule?: Record<string, unknown>[]; webhook?: Record<string, unknown>[] };
const triggers = () =>
  (parseDocument(yamlText()).toJS() as { spec: { triggers?: Triggers } }).spec.triggers;

async function open(source = SOURCE, diagnostics: Diagnostic[] = []) {
  server.use(
    ...editorHandlers({
      draft: { source, revision: 4 },
      latestVersion: 2,
      teams: [],
      catalog: [],
    }),
  );
  renderEditor(fakeCompiler(() => diagnostics));
  const editor = await screen.findByTestId('triggers-editor');
  return { user: userEvent.setup(), editor };
}

beforeEach(() => {
  tokenStore.set('trb_pat_test');
  rememberEditorTab('graph');
});
afterEach(() => tokenStore.clear());

describe('the builder: triggers', () => {
  it('adds a daily schedule and changes it with the form', async () => {
    const { user, editor } = await open();
    await user.click(within(editor).getByRole('button', { name: 'Add a schedule' }));
    await waitFor(() => expect(triggers()?.schedule).toHaveLength(1));
    const added = triggers()?.schedule?.[0];
    expect(added).toMatchObject({ id: 'daily', cron: '0 9 * * *' });
    expect(typeof added?.timezone).toBe('string');
    // Triggers sit after the inputs, before the steps.
    expect(yamlText().indexOf('triggers:')).toBeGreaterThan(yamlText().indexOf('inputs:'));
    expect(yamlText().indexOf('triggers:')).toBeLessThan(yamlText().indexOf('steps:'));

    const card = within(screen.getByTestId('schedule-trigger-0'));
    expect(card.getByTestId('schedule-summary')).toHaveTextContent('Every day at 9:00 AM');

    await user.selectOptions(card.getByLabelText('Repeats'), 'weekdays');
    await waitFor(() => expect(triggers()?.schedule?.[0]?.cron).toBe('0 9 * * 1-5'));
    fireEvent.change(card.getByLabelText('Time'), { target: { value: '08:30' } });
    await waitFor(() => expect(triggers()?.schedule?.[0]?.cron).toBe('30 8 * * 1-5'));
    expect(card.getByTestId('schedule-summary')).toHaveTextContent(
      'Every weekday (Monday to Friday) at 8:30 AM',
    );

    await user.selectOptions(card.getByLabelText('Repeats'), 'weekly');
    await user.selectOptions(card.getByLabelText('Day'), '5');
    await waitFor(() => expect(triggers()?.schedule?.[0]?.cron).toBe('30 8 * * 5'));

    await user.selectOptions(card.getByLabelText('Time zone'), 'America/New_York');
    await user.selectOptions(card.getByLabelText('If a run is missed'), '1h');
    await waitFor(() =>
      expect(triggers()?.schedule?.[0]).toMatchObject({
        timezone: 'America/New_York',
        catch_up: '1h',
      }),
    );
    expect(card.getByTestId('schedule-summary')).toHaveTextContent(
      'Every Friday at 8:30 AM (America/New York)',
    );
  });

  it('takes a custom cron expression and keeps the custom field open', async () => {
    const { user } = await open();
    await user.click(screen.getByRole('button', { name: 'Add a schedule' }));
    const card = within(await screen.findByTestId('schedule-trigger-0'));
    await user.selectOptions(card.getByLabelText('Repeats'), 'custom');
    const field = card.getByLabelText('Cron expression');
    await user.clear(field);
    await user.type(field, '*/15 * * * *');
    await waitFor(() => expect(triggers()?.schedule?.[0]?.cron).toBe('*/15 * * * *'));
    expect(card.getByTestId('schedule-summary')).toHaveTextContent('Every 15 minutes');
    // Typing a shape the presets know does not take the field away.
    await user.clear(card.getByLabelText('Cron expression'));
    await user.type(card.getByLabelText('Cron expression'), '0 2 * * *');
    expect(card.getByLabelText('Cron expression')).toBeInTheDocument();
    expect(card.getByTestId('schedule-summary')).toHaveTextContent('Every day at 2:00 AM');
  });

  it('gives inputs typed values, and the compiler’s problem shows under the field', async () => {
    const missing: Diagnostic = {
      path: '/spec/triggers/schedule/0/inputs/service',
      code: 'spec.trigger_input_invalid',
      message: 'A trigger must give every required input.',
      line: 9,
      column: 9,
    };
    const source = SOURCE.replace(
      '  steps:',
      '  triggers:\n    schedule:\n      - { id: nightly, cron: "0 2 * * *" }\n  steps:',
    );
    const { user } = await open(source, [missing]);
    const card = within(screen.getByTestId('schedule-trigger-0'));
    expect(card.getByTestId('schedule-summary')).toHaveTextContent('Every day at 2:00 AM (UTC)');
    await waitFor(() =>
      expect(card.getByText('A trigger must give every required input.')).toBeInTheDocument(),
    );
    expect(card.getByLabelText(/service/)).toHaveAccessibleName('service (required)');

    await user.type(card.getByLabelText(/service/), 'api');
    await user.type(card.getByLabelText(/replicas/), '3');
    await user.selectOptions(card.getByLabelText(/region/), 'eu');
    await waitFor(() =>
      expect(triggers()?.schedule?.[0]?.inputs).toEqual({
        service: 'api',
        replicas: 3,
        region: 'eu',
      }),
    );
    await user.clear(card.getByLabelText(/replicas/));
    await waitFor(() =>
      expect(triggers()?.schedule?.[0]?.inputs).toEqual({ service: 'api', region: 'eu' }),
    );
  });

  it('adds a webhook with its URL, payload, condition and inputs', async () => {
    const { user, editor } = await open();
    await user.click(within(editor).getByRole('button', { name: 'Add a webhook' }));
    const card = within(await screen.findByTestId('webhook-trigger-0'));
    await waitFor(() => expect(triggers()?.webhook).toEqual([{ id: 'incoming' }]));
    expect(
      card.getByText(/POST http.*\/v1\/projects\/prod\/runbooks\/post-deploy\/triggers\/incoming/),
    ).toBeInTheDocument();

    await user.click(card.getByRole('button', { name: 'Add a payload field' }));
    await user.type(
      card.getByLabelText('Only start a run when'),
      "${{{{ payload.status == 'ok' }}",
    );
    await user.type(card.getByLabelText(/service/), '${{{{ payload.repo }}');
    await waitFor(() =>
      expect(triggers()?.webhook?.[0]).toMatchObject({
        id: 'incoming',
        when: "${{ payload.status == 'ok' }}",
        inputs: { service: '${{ payload.repo }}' },
      }),
    );
    expect(Object.keys((triggers()?.webhook?.[0]?.payload as object) ?? {})).toHaveLength(1);
    await expectNoAxeViolations(screen.getByTestId('triggers-editor'));
  });

  it('removes triggers, leaving no empty triggers map', async () => {
    const source = SOURCE.replace(
      '  steps:',
      '  triggers:\n    schedule:\n      - { id: nightly, cron: "0 2 * * *" }\n      - { id: weekly, cron: "0 9 * * 1" }\n    webhook:\n      - id: on_deploy\n  steps:',
    );
    const { user } = await open(source);
    await user.click(screen.getByRole('button', { name: 'Remove schedule nightly' }));
    await waitFor(() => expect(triggers()?.schedule?.map((s) => s.id)).toEqual(['weekly']));
    await user.click(screen.getByRole('button', { name: 'Remove schedule weekly' }));
    await waitFor(() => expect(triggers()).toEqual({ webhook: [{ id: 'on_deploy' }] }));
    await user.click(screen.getByRole('button', { name: 'Remove webhook on_deploy' }));
    await waitFor(() => expect(triggers()).toBeUndefined());
    expect(yamlText()).not.toContain('triggers');
  });

  it('names new triggers so they do not clash', async () => {
    const source = SOURCE.replace(
      '  steps:',
      '  triggers:\n    webhook:\n      - id: daily\n  steps:',
    );
    const { user } = await open(source);
    await user.click(screen.getByRole('button', { name: 'Add a schedule' }));
    await waitFor(() => expect(triggers()?.schedule?.[0]?.id).toBe('daily_2'));
  });
});
