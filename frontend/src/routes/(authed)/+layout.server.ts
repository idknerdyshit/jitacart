import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

/**
 * Server-side auth gate for the (authed) route group. Forwards the user's
 * cookies to the backend's /api/me endpoint via SvelteKit's `event.fetch`.
 * On 200 we return the parsed body; on 401 we redirect before any HTML is
 * sent to the browser. Removing the client-only `ssr = false` lets this
 * gate run before hydration.
 */
export const load: LayoutServerLoad = async ({ fetch, request }) => {
    const cookie = request.headers.get('cookie') ?? '';
    const res = await fetch('/api/me', {
        headers: { cookie },
    });
    if (res.status === 401) {
        throw redirect(302, '/');
    }
    if (!res.ok) {
        // Surface non-401 errors so the user gets an error page, not a
        // misleading "you're logged in but everything is broken" experience.
        throw new Error(`/me responded ${res.status}`);
    }
    const me = await res.json();
    return { me };
};
