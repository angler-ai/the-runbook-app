import { defineGallery } from '../../gallery/registry';
import { NotFound } from './NotFound';
import { Forbidden } from './Forbidden';

export default [
  defineGallery({
    id: 'fallback-pages',
    group: 'Pages',
    title: 'Fallback Pages (404 / 403)',
    examples: [
      {
        name: '404 Not Found Page',
        render: () => (
          <div className="w-full min-h-[360px] bg-canvas flex items-center justify-center p-4">
            <NotFound
              renderLink={({ href, className, children }) => (
                <a
                  href={href}
                  onClick={(e) => {
                    e.preventDefault();
                    alert(`Navigating to: ${href}`);
                  }}
                  className={className}
                >
                  {children}
                </a>
              )}
            />
          </div>
        ),
      },
      {
        name: '403 Forbidden Access Denied Page',
        render: () => (
          <div className="w-full min-h-[360px] bg-canvas flex items-center justify-center p-4">
            <Forbidden
              renderLink={({ href, className, children }) => (
                <a
                  href={href}
                  onClick={(e) => {
                    e.preventDefault();
                    alert(`Navigating to: ${href}`);
                  }}
                  className={className}
                >
                  {children}
                </a>
              )}
            />
          </div>
        ),
      },
    ],
  }),
];
