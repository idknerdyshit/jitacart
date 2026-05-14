import { writable, derived, get, type Readable } from 'svelte/store';
import { browser } from '$app/environment';
import { api, type Me, type ViewerCharacter } from '$lib/api';

export type { Me, ViewerCharacter };

export const CONTRACTS_SCOPE = 'esi-contracts.read_character_contracts.v1';

// Module-level state — DELIBERATELY client-only. Under adapter-node a module
// singleton is shared across concurrent SSR requests, so a server-populated
// `meStore`/`inflight` would leak one user's identity into another's render.
// Server code must read request-scoped `event.locals.me` (see hooks.server.ts)
// instead; the `browser` guards below make that contract enforceable rather
// than merely conventional.
const meStore = writable<Me | null>(null);

let inflight: Promise<Me | null> | null = null;

export async function loadMe(force = false): Promise<Me | null> {
    if (!browser) {
        throw new Error('loadMe is client-only; server code must use event.locals.me');
    }
    if (!force) {
        const cached = get(meStore);
        if (cached) return cached;
    }
    if (inflight) return inflight;
    inflight = api<Me>('/me')
        .then((m) => {
            meStore.set(m);
            return m;
        })
        .catch((e: unknown) => {
            // 401 surfaces via api() which already redirects; the throw lets
            // us return null cleanly. Other errors (network/5xx) should
            // bubble — silently swallowing them masks real outages.
            const msg = e instanceof Error ? e.message : '';
            if (msg === 'unauthenticated') return null;
            throw e;
        })
        .finally(() => {
            inflight = null;
        });
    return inflight;
}

export function hydrateMe(m: Me | null): void {
    // No-op on the server: writing the shared singleton during SSR is the
    // exact cross-request leak we're guarding against. Callers hydrate from
    // an `$effect` (client-only) anyway.
    if (!browser) return;
    meStore.set(m);
}

export const me: Readable<Me | null> = meStore;

export const viewerCharacters: Readable<ViewerCharacter[]> = derived(
    meStore,
    ($me) => $me?.characters ?? []
);

export function characterHasContractsScope(c: ViewerCharacter): boolean {
    return c.scopes.includes(CONTRACTS_SCOPE);
}
