import { writable, derived, get, type Readable } from 'svelte/store';
import { api, type Me, type ViewerCharacter } from '$lib/api';

export type { Me, ViewerCharacter };

export const CONTRACTS_SCOPE = 'esi-contracts.read_character_contracts.v1';

const meStore = writable<Me | null>(null);

let inflight: Promise<Me | null> | null = null;

export async function loadMe(force = false): Promise<Me | null> {
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
        .catch(() => null)
        .finally(() => {
            inflight = null;
        });
    return inflight;
}

export const me: Readable<Me | null> = meStore;

export const viewerCharacters: Readable<ViewerCharacter[]> = derived(
    meStore,
    ($me) => $me?.characters ?? []
);

export function characterHasContractsScope(c: ViewerCharacter): boolean {
    return c.scopes.includes(CONTRACTS_SCOPE);
}
