<script lang="ts">
    import { onDestroy } from 'svelte';
    import { api } from '$lib/api';

    type Hit = {
        market_id: string;
        name: string;
        short_label: string | null;
        region_id: number | null;
        solar_system_id: number | null;
        structure_type_id: number | null;
        is_public: boolean;
        tracked: boolean;
    };

    let { groupId, onClose, onTracked }: {
        groupId: string;
        onClose: () => void;
        onTracked: () => void;
    } = $props();

    let q = $state('');
    let results = $state<Hit[]>([]);
    let busy = $state(false);
    let error = $state<string | null>(null);
    let timer: ReturnType<typeof setTimeout> | null = null;

    function debounce() {
        if (timer) clearTimeout(timer);
        timer = setTimeout(runSearch, 250);
    }

    onDestroy(() => {
        if (timer) clearTimeout(timer);
    });

    async function runSearch() {
        const term = q.trim();
        if (term.length === 0) {
            results = [];
            return;
        }
        busy = true;
        error = null;
        try {
            results = await api<Hit[]>(
                `/groups/${groupId}/markets/citadels/search?q=${encodeURIComponent(term)}`
            );
        } catch (e) {
            error = e instanceof Error ? e.message : String(e);
            results = [];
        } finally {
            busy = false;
        }
    }

    async function track(hit: Hit) {
        if (hit.tracked) return;
        busy = true;
        error = null;
        try {
            await api(`/groups/${groupId}/tracked-markets`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ market_id: hit.market_id })
            });
            results = results.map((r) =>
                r.market_id === hit.market_id ? { ...r, tracked: true } : r
            );
            onTracked();
        } catch (e) {
            error = e instanceof Error ? e.message : String(e);
        } finally {
            busy = false;
        }
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="overlay" onclick={onClose} role="presentation">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions a11y_interactive_supports_focus -->
    <div class="modal" onclick={(e) => e.stopPropagation()} role="dialog" aria-modal="true" tabindex="-1">
        <h2>Add a citadel</h2>
        <p class="muted">
            Searches public structures already discovered by ESI. If yours isn't here, a member
            with the universe scope needs to upgrade so we can resolve its details.
        </p>

        <!-- svelte-ignore a11y_autofocus -->
        <input
            type="text"
            placeholder="search by name (e.g. 1DQ1)"
            bind:value={q}
            oninput={debounce}
            autofocus
        />

        {#if error}<p class="error">{error}</p>{/if}
        {#if busy}<p class="muted">Searching…</p>{/if}

        <ul>
            {#each results as r (r.market_id)}
                <li>
                    <div>
                        <strong>{r.short_label ?? r.name}</strong>
                        <div class="muted">{r.name}</div>
                    </div>
                    {#if r.tracked}
                        <button disabled>Tracked</button>
                    {:else}
                        <button onclick={() => track(r)} disabled={busy}>Track</button>
                    {/if}
                </li>
            {/each}
            {#if results.length === 0 && q.trim() && !busy}
                <li class="muted">No matches.</li>
            {/if}
        </ul>

        <div class="actions">
            <button onclick={onClose}>Close</button>
        </div>
    </div>
</div>

<style>
    .overlay {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.55);
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 50;
    }
    .modal {
        background: #0d1117;
        border: 1px solid #30363d;
        border-radius: 10px;
        width: min(560px, 92vw);
        padding: 1.25rem;
        max-height: 80vh;
        overflow: auto;
    }
    input {
        width: 100%;
        padding: 0.5rem 0.65rem;
        background: #161b22;
        border: 1px solid #30363d;
        color: #e6edf3;
        border-radius: 6px;
        margin: 0.5rem 0 1rem;
    }
    ul {
        list-style: none;
        padding: 0;
        margin: 0;
    }
    li {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 1rem;
        padding: 0.5rem 0;
        border-bottom: 1px solid #21262d;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.3rem 0.7rem;
        border-radius: 6px;
        cursor: pointer;
    }
    .actions {
        margin-top: 1rem;
        display: flex;
        justify-content: flex-end;
    }
    .error {
        color: #f87171;
    }
    .muted {
        color: #8b949e;
        font-size: 0.9em;
    }
</style>
