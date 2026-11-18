import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { useState } from 'react';
import { retryQuery } from './api/query-policy';
import { router as defaultRouter } from './routes/router';
import { Toaster } from './ui/primitives/sonner';
import { TooltipProvider } from './ui/primitives/tooltip';

export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: retryQuery, refetchOnWindowFocus: true, staleTime: 5_000 },
      mutations: { retry: 0 },
    },
  });
}

export function App({ router = defaultRouter }: { router?: typeof defaultRouter }) {
  const [queryClient] = useState(createQueryClient);
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <RouterProvider router={router} />
        <Toaster />
      </TooltipProvider>
    </QueryClientProvider>
  );
}
