import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expectNoAxeViolations } from '../../test/axe';
import { NotFound } from './NotFound';
import { Forbidden } from './Forbidden';

describe('NotFound page', () => {
  it('renders 404 message and handles renderLink', async () => {
    const user = userEvent.setup();
    const customLinkClick = vi.fn();

    const { container } = render(
      <NotFound
        renderLink={({ href, className, children }) => (
          <button
            type="button"
            data-href={href}
            className={className}
            onClick={customLinkClick}
          >
            {children}
          </button>
        )}
      />,
    );

    expect(screen.getByText('Page not found')).toBeInTheDocument();
    expect(
      screen.getByText(
        "The page you are looking for doesn't exist, has been moved, or you don't have access to it.",
      ),
    ).toBeInTheDocument();

    const button = screen.getByRole('button', { name: 'Go to My tasks' });
    expect(button).toHaveAttribute('data-href', '/tasks');
    await user.click(button);
    expect(customLinkClick).toHaveBeenCalled();

    await expectNoAxeViolations(container);
  });

  it('renders default link when renderLink is not supplied', async () => {
    const { container } = render(<NotFound />);
    const link = screen.getByRole('link', { name: 'Go to My tasks' });
    expect(link).toHaveAttribute('href', '/tasks');
    await expectNoAxeViolations(container);
  });
});

describe('Forbidden page', () => {
  it('renders 403 message and handles renderLink', async () => {
    const user = userEvent.setup();
    const customLinkClick = vi.fn();

    const { container } = render(
      <Forbidden
        renderLink={({ href, className, children }) => (
          <button
            type="button"
            data-href={href}
            className={className}
            onClick={customLinkClick}
          >
            {children}
          </button>
        )}
      />,
    );

    expect(screen.getByText('Access denied')).toBeInTheDocument();
    expect(
      screen.getByText(
        'You do not have permission to view this resource. Contact your organization administrator for access.',
      ),
    ).toBeInTheDocument();

    const button = screen.getByRole('button', { name: 'Go to My tasks' });
    expect(button).toHaveAttribute('data-href', '/tasks');
    await user.click(button);
    expect(customLinkClick).toHaveBeenCalled();

    await expectNoAxeViolations(container);
  });

  it('renders default link when renderLink is not supplied', async () => {
    const { container } = render(<Forbidden />);
    const link = screen.getByRole('link', { name: 'Go to My tasks' });
    expect(link).toHaveAttribute('href', '/tasks');
    await expectNoAxeViolations(container);
  });
});
