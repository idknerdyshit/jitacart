<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import { api, type ViewerCharacter } from '$lib/api';
    import { viewerCharacters } from '$lib/stores/me';
    import CitadelSearchModal from '$lib/CitadelSearchModal.svelte';

    type Tracked = {
        market_id: string;
        name: string | null;
        short_label: string | null;
        region_id: number | null;
        solar_system_id: number | null;
        structure_type_id: number | null;
        is_public: boolean;
        last_orders_synced_at: string | null;
        accessing_character_id: string | null;
        accessing_character_name: string | null;
        untrackable_until: string | null;
    };

    type GroupDetail = {
        group: { id: string; name: string };
        role: 'owner' | 'member';
    };

    const REQUIRED_SCOPES = [
        'esi-markets.structure_markets.v1',
        'esi-universe.read_structures.v1'
    ];

    const groupId = $derived(page.params.id ?? '');
    let detail = $state<GroupDetail | null>(null);
    const characters = $derived($viewerCharacters);
    let tracked = $state<Tracked[]>([]);
    let error = $state<string | null>(null);
    let modalOpen = $state(false);

    async function load() {
        error = null;
        try {
            detail = await api<GroupDetail>(`/groups/${groupId}`);
            if (detail.role === 'owner') {
                tracked = await api<Tracked[]>(`/groups/${groupId}/tracked-markets`);
            }
        } catch (e) {
            error = e instanceof Error ? e.message : String(e);
        }
    }

    onMount(load);

    async function untrack(mid: string) {
        if (!confirm('Stop tracking this citadel? Lists referencing it keep their saved markets.')) {
            return;
        }
        try {
            await api(`/groups/${groupId}/tracked-markets/${mid}`, { method: 'DELETE' });
            tracked = tracked.filter((t) => t.market_id !== mid);
        } catch (e) {
            error = e instanceof Error ? e.message : String(e);
        }
    }

    function missingScopes(c: ViewerCharacter): string[] {
        return REQUIRED_SCOPES.filter((s) => !c.scopes.includes(s));
    }

    function upgradeUrl(c: ViewerCharacter): string {
        return `/api/auth/eve/upgrade?character_id=${c.character_id}&scopes=${REQUIRED_SCOPES.join(
            ','
        )}&return_to=${encodeURIComponent(`/groups/${groupId}/tracked-markets`)}`;
    }

    function fmtTime(s: string | null): string {
        if (!s) return '—';
        return new Date(s).toLocaleString();
    }
</script>

<p><a href={`/groups/${groupId}`}>← Group</a></p>

{#if error}<p class="error">{error}</p>{/if}

{#if detail}
    <h1>Tracked citadels — {detail.group.name}</h1>

    {#if detail.role !== 'owner'}
        <p>Only group owners can manage the tracked-citadel set.</p>
    {:else}
        {#if characters.length > 0}
            <section>
                <h2>Character access</h2>
                <p class="muted">
                    Tracking a citadel requires at least one linked character with structure
                    scopes that can dock at it.
                </p>
                <ul>
                    {#each characters as c (c.id)}
                        {@const miss = missingScopes(c)}
                        <li>
                            <strong>{c.character_name}</strong>
                            {#if miss.length === 0}
                                <span class="ok">all required scopes granted</span>
                            {:else}
                                <span class="warn">missing: {miss.join(', ')}</span>
                                <a class="btn" href={upgradeUrl(c)}>Grant market access</a>
                            {/if}
                        </li>
                    {/each}
                </ul>
            </section>
        {/if}

        <section>
            <h2>Tracked citadels</h2>
            <button class="primary" onclick={() => (modalOpen = true)}>+ Add citadel</button>
            {#if tracked.length === 0}
                <p class="muted">None yet. Use the search above to track a public citadel.</p>
            {:else}
                <table>
                    <thead>
                        <tr>
                            <th>Citadel</th>
                            <th>Last orders sync</th>
                            <th>Accessing character</th>
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each tracked as t (t.market_id)}
                            <tr class:stale={!t.is_public}>
                                <td>
                                    <strong>{t.short_label ?? t.name ?? '(unnamed)'}</strong>
                                    {#if t.short_label && t.name}<br /><span class="muted">{t.name}</span>{/if}
                                    {#if !t.is_public}<br /><span class="warn">no longer public</span>{/if}
                                    {#if t.untrackable_until}
                                        <br /><span class="warn">paused until {fmtTime(t.untrackable_until)}</span>
                                    {/if}
                                </td>
                                <td>{fmtTime(t.last_orders_synced_at)}</td>
                                <td>{t.accessing_character_name ?? '—'}</td>
                                <td>
                                    <button class="danger" onclick={() => untrack(t.market_id)}>Untrack</button>
                                </td>
                            </tr>
                        {/each}
                    </tbody>
                </table>
            {/if}
        </section>

        {#if modalOpen}
            <CitadelSearchModal
                {groupId}
                onClose={() => (modalOpen = false)}
                onTracked={async () => {
                    tracked = await api<Tracked[]>(`/groups/${groupId}/tracked-markets`);
                }}
            />
        {/if}
    {/if}
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    .error {
        color: #f87171;
    }
    .muted {
        color: #8b949e;
    }
    .ok {
        color: #4ade80;
        font-size: 0.9em;
    }
    .warn {
        color: #fbbf24;
        font-size: 0.9em;
    }
    table {
        width: 100%;
        border-collapse: collapse;
        margin-top: 0.5rem;
    }
    th,
    td {
        text-align: left;
        padding: 0.5rem;
        border-bottom: 1px solid #21262d;
    }
    tr.stale {
        opacity: 0.5;
    }
    button,
    .btn {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.3rem 0.7rem;
        border-radius: 6px;
        cursor: pointer;
        text-decoration: none;
        font-size: 0.95em;
        margin-left: 0.5rem;
    }
    button.primary {
        background: #1f6feb;
        border-color: #1f6feb;
        color: white;
    }
    button.danger {
        border-color: #6e2832;
        color: #f87171;
    }
</style>
