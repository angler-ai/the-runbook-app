// UI-1: applied before first paint (a classic script in <head>, allowed by
// the CSP's script-src 'self'). It mirrors src/theme/boot.ts: the person's
// light/dark/system choice and the operator's cached colour override, so a
// reload never flashes the default theme. main.tsx refreshes the override
// from GET /v1/meta before the app renders.
//
// The cache (`rb.theme-vars`) holds colours, not CSS. Every name and value is
// validated here again (`--rb-…` names, `#rrggbb` values) and anything else
// is dropped; the CSS is rebuilt with the selectors cssFromVars uses
// (src/theme/colors.ts), and theme.test.ts checks the two agree.
(function () {
  var root = document.documentElement;
  var NAME = /^--rb-[a-z0-9-]+$/;
  var VALUE = /^#[0-9a-f]{6}$/;
  var preference = 'light';
  var cached = null;
  try {
    var stored = window.localStorage.getItem('rb.theme');
    if (stored === 'light' || stored === 'dark' || stored === 'system') preference = stored;
    // The old raw-CSS cache is never applied.
    window.localStorage.removeItem('rb.theme-css');
    cached = JSON.parse(window.localStorage.getItem('rb.theme-vars') || 'null');
  } catch {
    // Storage blocked or the cache unreadable: the defaults apply.
  }
  if (preference === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', preference);

  function body(vars) {
    var out = '';
    if (!vars || typeof vars !== 'object' || Array.isArray(vars)) return out;
    var names = Object.keys(vars);
    for (var i = 0; i < names.length; i++) {
      var name = names[i];
      var value = vars[name];
      if (NAME.test(name) && typeof value === 'string' && VALUE.test(value))
        out += name + ':' + value + ';';
    }
    return out;
  }

  var parts = [];
  if (cached && typeof cached === 'object') {
    var light = body(cached.light);
    var dark = body(cached.dark);
    if (light) parts.push(':root{' + light + '}');
    if (dark) {
      parts.push(
        '@media screen and (prefers-color-scheme: dark){:root:not([data-theme="light"]){' +
          dark +
          '}}',
        '@media screen{:root[data-theme="dark"]{' + dark + '}}',
      );
    }
  }
  if (parts.length) {
    var style = document.createElement('style');
    style.id = 'rb-theme-override';
    style.textContent = parts.join('\n');
    document.head.appendChild(style);
  }
})();
