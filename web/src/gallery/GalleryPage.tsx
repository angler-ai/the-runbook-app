import { galleryEntries } from './entries';
import type { GalleryEntry, GalleryExample } from './registry';

const THEMES = ['light', 'dark'] as const;

/** Each pane is its own component instance, so examples get separate state. */
function ExamplePane({ render }: Pick<GalleryExample, 'render'>) {
  return <>{render()}</>;
}

function selectedEntries(): GalleryEntry[] {
  const only = new URLSearchParams(window.location.search).get('entry');
  return only ? galleryEntries.filter((e) => e.id === only) : galleryEntries;
}

/**
 * Dev-only component gallery (A11). Each example renders twice, in a light and a
 * dark pane (`data-theme` on the pane). Portaled content (dialogs, menus, toasts)
 * renders at the document root and follows the page theme, not the pane.
 * `?entry=<id>` shows a single entry, for screenshots.
 */
export default function GalleryPage() {
  const entries = selectedEntries();
  const groups = [...new Set(entries.map((e) => e.group))];
  return (
    <div className="min-h-dvh bg-canvas text-fg">
      <header className="border-b border-border px-4 py-3">
        <h1 className="text-xl font-semibold">Component gallery</h1>
        <p className="text-xs text-fg-muted">
          Development only. {galleryEntries.length} registered entries.
        </p>
      </header>
      <div className="flex flex-col md:flex-row">
        <nav aria-label="Gallery entries" className="border-border p-4 md:w-60 md:border-r">
          {groups.map((group) => (
            <div key={group} className="mb-4">
              <h2 className="mb-1 text-2xs font-medium tracking-wide text-fg-muted uppercase">
                {group}
              </h2>
              <ul>
                {entries
                  .filter((e) => e.group === group)
                  .map((e) => (
                    <li key={e.id}>
                      <a className="text-base text-accent hover:underline" href={`#${e.id}`}>
                        {e.title}
                      </a>
                    </li>
                  ))}
              </ul>
            </div>
          ))}
        </nav>
        <main className="min-w-0 flex-1 p-4">
          {entries.length === 0 ? <p>No gallery entries match.</p> : null}
          {entries.map((entry) => (
            <section
              key={entry.id}
              id={entry.id}
              aria-labelledby={`${entry.id}-title`}
              className="mb-10"
            >
              <h2 id={`${entry.id}-title`} className="mb-3 text-md font-semibold">
                {entry.title}
              </h2>
              {entry.examples.map((example) => (
                <div key={example.name} className="mb-6">
                  <h3 className="mb-2 text-xs font-medium text-fg-muted">{example.name}</h3>
                  <div className="grid gap-px overflow-hidden rounded-md border border-border bg-border lg:grid-cols-2">
                    {THEMES.map((theme) => (
                      <div
                        key={theme}
                        data-theme={theme}
                        data-gallery-pane={`${entry.id}/${example.name}/${theme}`}
                        className="bg-canvas p-4 text-fg"
                      >
                        <ExamplePane render={example.render} />
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </section>
          ))}
        </main>
      </div>
    </div>
  );
}
