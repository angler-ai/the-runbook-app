import { createMemoryHistory } from '@tanstack/react-router';
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from './App';
import { createAppRouter } from './routes/router';
import { expectNoAxeViolations } from './test/axe';

function renderAt(path: string) {
  const router = createAppRouter(createMemoryHistory({ initialEntries: [path] }));
  return { router, ...render(<App router={router} />) };
}

describe('App', () => {
  it('redirects / to the sign-in placeholder', async () => {
    const { router, container } = renderAt('/');
    expect(await screen.findByRole('heading', { name: 'Sign in' })).toBeInTheDocument();
    expect(router.state.location.pathname).toBe('/sign-in');
    await expectNoAxeViolations(container);
  });

  it('serves the gallery at /__gallery in development', async () => {
    renderAt('/__gallery');
    expect(await screen.findByRole('heading', { name: 'Component gallery' })).toBeInTheDocument();
  });

  it('shows a not-found page for unknown paths', async () => {
    renderAt('/nope');
    expect(await screen.findByRole('heading', { name: 'Page not found' })).toBeInTheDocument();
  });
});
