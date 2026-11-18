// The invite dialog says what each organization role sees: a member only the
// projects granted below, an admin or owner the organization's settings but
// a project's runbooks and runs only with a project role.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { OrgProvider } from '../context/OrgContext';
import { InviteMemberDialog } from './InviteMemberDialog';

afterEach(() => tokenStore.clear());

describe('the invite dialog', () => {
  it('says what each role sees', async () => {
    tokenStore.set('trb_pat_test');
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <OrgProvider>
          <InviteMemberDialog open onOpenChange={() => {}} />
        </OrgProvider>
      </QueryClientProvider>,
    );
    const hint = await screen.findByTestId('invite-role-hint');
    expect(hint).toHaveTextContent('Sees only the projects you grant below.');
    const user = userEvent.setup();
    await user.click(screen.getByRole('combobox', { name: 'Organization role' }));
    await user.click(await screen.findByRole('option', { name: 'Admin' }));
    expect(hint).toHaveTextContent(/only with a project role/);
  });
});
