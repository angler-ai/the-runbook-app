import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../../test/axe';
import { server } from '../../test/msw/server';
import { DeviceApprovalPage } from './DeviceApprovalPage';

const sampleLookup = {
  user_code: 'WDJB-MJHT',
  client_name: 'runbook CLI on laptop',
  client_name_verified: false,
  requester_ip: '198.51.100.7',
  requested_scopes: ['runbooks:read', 'executions:start', 'executions:act'],
  created_at_ms: Date.now() - 60000,
  expires_at_ms: Date.now() + 540000,
};

describe('DeviceApprovalPage', () => {
  it('looks up code, shows unverified client claim and scopes, and approves device', async () => {
    let approveCalled = false;
    server.use(
      http.post('*/v1/device/lookup', async ({ request }) => {
        const body = (await request.json()) as { user_code: string };
        expect(body.user_code).toBe('WDJB-MJHT');
        return HttpResponse.json(sampleLookup);
      }),
      http.post('*/v1/device/approve', async ({ request }) => {
        approveCalled = true;
        const body = (await request.json()) as { user_code: string; approve: boolean };
        expect(body.user_code).toBe('WDJB-MJHT');
        expect(body.approve).toBe(true);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    const user = userEvent.setup();
    const { container } = render(<DeviceApprovalPage />);

    // Enter user code manually
    const input = screen.getByLabelText(/confirmation code/i);
    await user.type(input, 'WDJB-MJHT');
    await user.click(screen.getByRole('button', { name: /look up device/i }));

    // Details displayed
    expect(await screen.findByText('WDJB-MJHT')).toBeInTheDocument();
    expect(screen.getByText(/runbook CLI on laptop/)).toBeInTheDocument();
    expect(screen.getByText(/(claimed by client)/i)).toBeInTheDocument();
    expect(screen.getByText(/198.51.100.7/)).toBeInTheDocument();
    expect(screen.getByText('runbooks:read')).toBeInTheDocument();
    expect(screen.getByText('executions:start')).toBeInTheDocument();

    // Approve
    await user.click(screen.getByRole('button', { name: /approve device/i }));
    expect(approveCalled).toBe(true);
    expect(await screen.findByText(/device approved/i)).toBeInTheDocument();
    expect(screen.getByText(/you may close this tab/i)).toBeInTheDocument();

    await expectNoAxeViolations(container);
  });

  it('denies device login when Deny button is clicked', async () => {
    let denyCalled = false;
    server.use(
      http.post('*/v1/device/lookup', () => HttpResponse.json(sampleLookup)),
      http.post('*/v1/device/approve', async ({ request }) => {
        denyCalled = true;
        const body = (await request.json()) as { user_code: string; approve: boolean };
        expect(body.approve).toBe(false);
        return new HttpResponse(null, { status: 204 });
      }),
    );

    const user = userEvent.setup();
    const { container } = render(<DeviceApprovalPage />);

    await user.type(screen.getByLabelText(/confirmation code/i), 'WDJB-MJHT');
    await user.click(screen.getByRole('button', { name: /look up device/i }));

    expect(await screen.findByRole('button', { name: /deny/i })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /deny/i }));

    expect(denyCalled).toBe(true);
    expect(await screen.findByText(/request denied/i)).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });

  it('displays error when confirmation code is invalid or expired', async () => {
    server.use(
      http.post('*/v1/device/lookup', () =>
        HttpResponse.json(
          { code: 'device.not_found', message: 'Not found' },
          { status: 404 },
        ),
      ),
    );

    const user = userEvent.setup();
    const { container } = render(<DeviceApprovalPage />);

    await user.type(screen.getByLabelText(/confirmation code/i), 'BAD-CODE');
    await user.click(screen.getByRole('button', { name: /look up device/i }));

    expect(
      await screen.findByText(/that confirmation code was not found or has expired/i),
    ).toBeInTheDocument();
    await expectNoAxeViolations(container);
  });
});
