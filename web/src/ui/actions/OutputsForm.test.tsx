// OutputsForm fixes from the 2026-11-11 UI review: errors tied to their
// fields (U22), list/object/unknown shapes (U24), server error paths under
// any root, and the controlled mode the start dialog uses (U9).
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import {
  fieldKeyOf,
  initialDraft,
  OutputsForm,
  validateDraft,
  type FormDraft,
  type JsonSchema,
} from './OutputsForm';

const schema: JsonSchema = {
  type: 'object',
  required: ['service', 'regions'],
  properties: {
    service: { type: 'string', title: 'Service', description: 'The service to verify' },
    count: { type: 'integer', title: 'Count' },
    regions: { type: 'array', title: 'Regions', items: { type: 'string' } },
    limits: { type: 'object', title: 'Limits', properties: { cpu: { type: 'integer' } } },
    env: { type: 'string', title: 'Environment', enum: ['prod', 'stage'] },
  },
};

describe('OutputsForm accessibility (U22)', () => {
  it('ties errors and descriptions to their fields and marks required fields', async () => {
    const { container } = render(
      <OutputsForm
        schema={schema}
        errors={[
          { path: 'inputs.service', message: 'unknown service' },
          { path: 'fields.env', message: 'not an option' },
        ]}
      />,
    );
    const service = screen.getByRole('textbox', { name: /service/i });
    expect(service).toHaveAttribute('aria-invalid', 'true');
    expect(service).toHaveAttribute('aria-required', 'true');
    expect(service).toHaveAccessibleDescription(/the service to verify.*unknown service/i);
    const env = screen.getByRole('combobox', { name: /environment/i });
    expect(env).toHaveAttribute('aria-invalid', 'true');
    expect(env).toHaveAccessibleDescription('not an option');
    const count = screen.getByRole('textbox', { name: /count/i });
    expect(count).not.toHaveAttribute('aria-invalid');
    expect(count).not.toHaveAttribute('aria-required');
    await expectNoAxeViolations(container);
  });

  it('moves focus to the first invalid field when its own submit fails', async () => {
    const user = userEvent.setup();
    render(<OutputsForm schema={schema} onSubmit={() => {}} />);
    await user.type(screen.getByRole('textbox', { name: /count/i }), 'x');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    const service = screen.getByRole('textbox', { name: /service/i });
    expect(service).toHaveFocus();
    expect(service).toHaveAccessibleDescription(/this field is required/i);
  });

  it('hides a server error once its field is edited', async () => {
    const user = userEvent.setup();
    render(<OutputsForm schema={schema} errors={[{ path: 'inputs.count', message: 'too big' }]} />);
    expect(screen.getByText('too big')).toBeInTheDocument();
    await user.type(screen.getByRole('textbox', { name: /count/i }), '3');
    expect(screen.queryByText('too big')).not.toBeInTheDocument();
  });
});

describe('OutputsForm error paths', () => {
  it.each([
    ['service', 'service'],
    ['inputs.service', 'service'],
    ['fields.note', 'note'],
    ['outputs.status_code', 'status_code'],
    ['inputs.regions[1]', 'regions'],
    ['inputs.limits.cpu', 'limits'],
    ['inputs["odd key"]', 'odd key'],
    ['inputs["odd key"].x', 'odd key'],
  ])('maps %s to the %s field', (path, key) => {
    expect(fieldKeyOf(path)).toBe(key);
  });
});

describe('OutputsForm schema shapes (U24)', () => {
  it('edits a required list and an object as JSON and submits the parsed values', async () => {
    const user = userEvent.setup();
    let submitted: Record<string, unknown> | null = null;
    render(
      <OutputsForm
        schema={schema}
        initialValues={{ service: 'api', limits: { cpu: 2 } }}
        onSubmit={(values) => (submitted = values)}
      />,
    );
    const regions = screen.getByRole('textbox', { name: /regions/i });
    expect(screen.getByRole('textbox', { name: /limits/i })).toHaveValue('{\n  "cpu": 2\n}');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(regions).toHaveAccessibleDescription(/this field is required/i);
    expect(submitted).toBeNull();

    await user.type(regions, '{{"us": 1}');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(regions).toHaveAccessibleDescription(/must be a json list/i);

    await user.clear(regions);
    await user.type(regions, '[[');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(regions).toHaveAccessibleDescription(/must be valid json/i);

    await user.clear(regions);
    await user.type(regions, '[["us-west", "eu-west"]');
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(submitted).toEqual({
      service: 'api',
      regions: ['us-west', 'eu-west'],
      limits: { cpu: 2 },
    });
  });

  it('keeps an unsupported field’s given value instead of dropping it', async () => {
    const user = userEvent.setup();
    let submitted: Record<string, unknown> | null = null;
    const odd: JsonSchema = {
      type: 'object',
      required: ['blob'],
      properties: { blob: { type: 'custom', title: 'Blob' }, note: { type: 'string' } },
    };
    render(
      <OutputsForm
        schema={odd}
        initialValues={{ blob: { kept: true } }}
        onSubmit={(values) => (submitted = values)}
      />,
    );
    expect(screen.getByText(/not supported in this form/i)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Submit outputs' }));
    expect(submitted).toEqual({ blob: { kept: true } });
  });

  it('refuses a required unsupported field that has no value', () => {
    const odd: JsonSchema = {
      type: 'object',
      required: ['blob'],
      properties: { blob: { type: 'custom' } },
    };
    expect(validateDraft(odd, initialDraft(odd)).errors).toEqual({
      blob: 'This value cannot be entered in this form.',
    });
  });
});

describe('OutputsForm controlled mode (U9)', () => {
  function Harness({ onDraft }: { onDraft(draft: FormDraft): void }) {
    const [draft, setDraft] = useState(() => initialDraft(schema));
    return (
      <OutputsForm
        schema={schema}
        values={draft}
        onValuesChange={(next) => {
          setDraft(next);
          onDraft(next);
        }}
      />
    );
  }

  it('reports every edit to the parent, whose draft validates to exactly what is shown', async () => {
    const user = userEvent.setup();
    let latest: FormDraft = {};
    render(<Harness onDraft={(draft) => (latest = draft)} />);
    await user.type(screen.getByRole('textbox', { name: /service/i }), 'web');
    await user.type(screen.getByRole('textbox', { name: /count/i }), '7');
    await user.type(screen.getByRole('textbox', { name: /regions/i }), '[["a"]');
    expect(validateDraft(schema, latest)).toEqual({
      values: { service: 'web', count: 7, regions: ['a'] },
      errors: {},
    });
  });
});
