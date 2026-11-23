// Render a component inside a minimal router, for components that use <Link>
// or router hooks outside the full app route tree.
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from '@tanstack/react-router';
import type { ReactNode } from 'react';

export function TestRouter({ children, path = '/' }: { children: ReactNode; path?: string }) {
  const rootRoute = createRootRoute({
    component: () => children,
    notFoundComponent: () => children,
  });
  const router = createRouter({
    routeTree: rootRoute,
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  return <RouterProvider router={router} />;
}
