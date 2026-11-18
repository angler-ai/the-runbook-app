import '@fontsource/dm-sans/400.css';
import '@fontsource/dm-sans/500.css';
import '@fontsource/dm-sans/600.css';
import '@fontsource/ibm-plex-mono/400.css';
import '@fontsource/ibm-plex-mono/500.css';
import './styles/index.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { applyPreference, loadThemeOverride, readPreference } from './theme';

const root = document.getElementById('root');
if (!root) throw new Error('index.html is missing #root');

// public/theme-boot.js has applied the cached theme before first paint; this
// repeats the preference (harmless) and refreshes the operator's override
// from /v1/meta before anything renders, so no default accent flashes (UI-1).
applyPreference(readPreference());
void loadThemeOverride().finally(() => {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
});
