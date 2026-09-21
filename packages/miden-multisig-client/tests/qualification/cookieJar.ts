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

    // A redirect would carry the `cookie` header to wherever it points, and a
    // session cookie is exactly what should not follow one. Every endpoint this
    // jar talks to is a local GUARDIAN that has no reason to redirect, so an
    // unexpected one is worth failing on rather than following.
    const response = await fetch(input, { ...init, headers, redirect: 'error' });

    // Named rather than optional: without it every `set-cookie` is dropped and
    // the operator scenarios fail as unauthorized, which reads as a product
    // defect. Node has had it since 19.7; a runtime that does not is a harness
    // problem and says so.
    if (typeof response.headers.getSetCookie !== 'function') {
      throw new Error(
        'this runtime cannot read set-cookie headers, so the operator session cannot be kept',
      );
    }
    const setCookie = response.headers.getSetCookie();
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
