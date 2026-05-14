import { redirect, error } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

/**
 * Server-side auth gate for the (authed) route group. `hooks.server.ts`
 * has already resolved `/api/me` into request-scoped `locals`. We gate on
 * that here, before any HTML is sent:
 *  - authenticated  -> hand the payload to the page tree as `data.me`
 *  - 403            -> account disabled/banned: show a dedicated error page
 *  - 401 / no cookie -> redirect to the landing page
 *  - anything else  -> surface as an error rather than a misleading
 *    "logged in but everything is broken" render
 */
export const load: LayoutServerLoad = async ({ locals }) => {
    if (locals.me) {
        return { me: locals.me };
    }
    if (locals.meStatus === 403) {
        throw error(
            403,
            'Your account is disabled. Contact your group owner if you think this is a mistake.'
        );
    }
    if (locals.meStatus === 401 || locals.meStatus === 0) {
        throw redirect(302, '/');
    }
    throw error(502, `/me responded ${locals.meStatus}`);
};
