import { derived, get } from 'svelte/store';
import { me, loadMe, type ViewerCharacter } from './me';
import { api } from '$lib/api';

export const activeCharacter = derived<typeof me, ViewerCharacter | null>(me, ($me) => {
    if (!$me) return null;
    if (!$me.active_character_id) return null;
    return $me.characters.find((c) => c.id === $me.active_character_id) ?? null;
});

export async function setActiveCharacter(characterId: string | null): Promise<void> {
    await api('/me/active-character', {
        method: 'PATCH',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ character_id: characterId })
    });
    await loadMe(true);
}
