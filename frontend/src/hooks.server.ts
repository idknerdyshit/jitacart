import type { Handle } from '@sveltejs/kit';
import type { Me } from '$lib/api';

/**
 * Per-request auth resolution. Fetches `/api/me` once with the request's
 * cookies and stashes the result on `event.locals`, so route loads read
 * request-scoped state instead of a module-level singleton (which
 * adapter-node would otherwise share across concurrent SSR requests).
 */
export const handle: Handle = async ({ event, resolve }) => {
    const cookie = event.request.headers.get('cookie') ?? '';
    let me: Me | null = null;
    let meStatus = 0;
    if (cookie) {
        const res = await event.fetch('/api/me', { headers: { cookie } });
        meStatus = res.status;
        if (res.ok) {
            me = (await res.json()) as Me;
        }
    }
    event.locals.me = me;
    event.locals.meStatus = meStatus;
    return resolve(event);
};
