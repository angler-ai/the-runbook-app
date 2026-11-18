import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { http, HttpResponse } from 'msw';
import { describe, expect, it } from 'vitest';
import { server } from '../../test/msw/server';
import { UnsubscribePage } from './UnsubscribePage';

describe('UnsubscribePage', () => {
  it('confirms before unsubscribing, then says it worked', async () => {
    server.use(http.post('*/v1/unsubscribe', () => HttpResponse.json({ unsubscribed: true })));
    render(<UnsubscribePage token="usr_1.abc" />);
    await userEvent.click(screen.getByRole('button', { name: 'Unsubscribe' }));
    expect(await screen.findByRole('status')).toHaveTextContent('You are unsubscribed');
  });

  it('says a refused link is not valid', async () => {
    server.use(
      http.post('*/v1/unsubscribe', () =>
        HttpResponse.json({ code: 'unsubscribe.invalid_token', status: 404 }, { status: 404 }),
      ),
    );
    render(<UnsubscribePage token="usr_1.bad" />);
    await userEvent.click(screen.getByRole('button', { name: 'Unsubscribe' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('not valid');
  });

  it('says a link without a token is not valid', () => {
    render(<UnsubscribePage />);
    expect(screen.getByRole('alert')).toHaveTextContent('not valid');
  });
});
