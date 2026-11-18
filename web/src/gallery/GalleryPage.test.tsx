import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { expectNoAxeViolations } from '../test/axe';
import { galleryEntries } from './entries';
import GalleryPage from './GalleryPage';

describe('GalleryPage', () => {
  it('discovers *.gallery.tsx files and renders each example in light and dark', async () => {
    expect(galleryEntries.map((e) => e.id)).toContain('primitives-button');
    const { container } = render(<GalleryPage />);
    const section = screen.getByRole('region', { name: 'Button' });
    const panes = section.querySelectorAll('[data-gallery-pane]');
    expect([...panes].map((p) => p.getAttribute('data-theme'))).toEqual(['light', 'dark']);
    for (const pane of panes) {
      expect(within(pane as HTMLElement).getByRole('button', { name: 'Roll back' })).toBeVisible();
    }
    await expectNoAxeViolations(container);
    // Renders every gallery example in both themes plus an axe pass: allow
    // for a loaded CI machine.
  }, 30_000);
});
