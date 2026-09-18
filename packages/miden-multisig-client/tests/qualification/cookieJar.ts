/**
 * The operator session is a cookie. A browser keeps one per origin; Node's
 * fetch does not, so a session established by `verify` would be dropped before
 * the next request and every authenticated call would read as unauthorized.
 */
export function fetchWithCookieJar(): typeof fetch {
  const jar = new Map<string, string>();

  return async function fetchWithCookies(input, init) {
    const headers = new Headers(init?.headers);
    if (jar.size > 0) {
      headers.set(
        'cookie',
        [...jar.entries()].map(([name, value]) => `${name}=${value}`).join('; '),
      );
    }

    const response = await fetch(input, { ...init, headers });

    const setCookie = response.headers.getSetCookie?.() ?? [];
    for (const cookie of setCookie) {
      const [pair] = cookie.split(';');
      const separator = pair.indexOf('=');
      if (separator <= 0) continue;
      const name = pair.slice(0, separator).trim();
      const value = pair.slice(separator + 1).trim();
      if (value === '' || /expires=thu, 01 jan 1970/i.test(cookie)) {
        jar.delete(name);
      } else {
        jar.set(name, value);
      }
    }

    return response;
  };
}
