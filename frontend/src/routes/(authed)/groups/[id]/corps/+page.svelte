<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import { api, fmtIsk, type Corp, type CorpAmbassador, type CorpWalletDivision } from '$lib/api';

    type CorpRow = Corp & {
        ambassadors: CorpAmbassador[];
        wallet_divisions: CorpWalletDivision[];
        is_ambassador: boolean;
    };

    type CorpsResponse = {
        corps: CorpRow[];
        role: 'owner' | 'member';
    };

    const groupId = $derived(page.params.id);

    let data = $state<CorpsResponse | null>(null);
    let error = $state<string | null>(null);

    // Link form
    let linking = $state(false);
    let linkCharId = $state('');
    let linkErr = $state<string | null>(null);

    // Add ambassador form (per corp)
    let addingAmbFor = $state<string | null>(null);
    let addAmbCharId = $state('');
    let addAmbErr = $state<string | null>(null);

    async function load() {
        error = null;
        try {
            data = await api<CorpsResponse>(`/groups/${groupId}/corps`);
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(load);

    async function linkCorp() {
        if (!linkCharId.trim()) return;
        const charId = Number(linkCharId.trim());
        if (!Number.isFinite(charId) || charId <= 0) {
            linkErr = 'Enter a valid EVE character ID.';
            return;
        }
        linkErr = null;
        linking = true;
        try {
            await api(`/groups/${groupId}/corps/link`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ character_id: charId })
            });
            linkCharId = '';
            await load();
        } catch (e) {
            linkErr = (e as Error).message;
        } finally {
            linking = false;
        }
    }

    async function unlinkCorp(corpId: string, name: string) {
        if (!confirm(`Unlink corp "${name}"? Ambassadors contributed by this group will be disabled.`)) return;
        try {
            await api(`/groups/${groupId}/corps/${corpId}`, { method: 'DELETE' });
            await load();
        } catch (e) {
            error = (e as Error).message;
        }
    }

    async function addAmbassador(corpId: string) {
        if (!addAmbCharId.trim()) return;
        const charId = Number(addAmbCharId.trim());
        if (!Number.isFinite(charId) || charId <= 0) {
            addAmbErr = 'Enter a valid EVE character ID.';
            return;
        }
        addAmbErr = null;
        try {
            await api(`/groups/${groupId}/corps/${corpId}/ambassadors`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ character_id: charId })
            });
            addAmbCharId = '';
            addingAmbFor = null;
            await load();
        } catch (e) {
            addAmbErr = (e as Error).message;
        }
    }

    async function removeAmbassador(corpId: string, characterId: string, name: string) {
        if (!confirm(`Remove ambassador "${name}"?`)) return;
        try {
            await api(`/groups/${groupId}/corps/${corpId}/ambassadors/${characterId}`, {
                method: 'DELETE'
            });
            await load();
        } catch (e) {
            error = (e as Error).message;
        }
    }
</script>

<p><a href={`/groups/${groupId}`}>← Group</a></p>

<h1>Corp wallets</h1>

{#if error}
    <p class="err">{error}</p>
{/if}

{#if data}
    {#if data.corps.length === 0}
        <p class="muted">No corps linked yet.</p>
    {/if}

    {#each data.corps as corp (corp.id)}
        <section class="corp-card">
            <div class="corp-header">
                <div>
                    <strong>{corp.name}</strong>
                    {#if corp.ticker}
                        <span class="ticker">[{corp.ticker}]</span>
                    {/if}
                    <span class="muted small">EVE corp #{corp.esi_corporation_id}</span>
                </div>
                {#if data.role === 'owner'}
                    <button class="danger small" onclick={() => unlinkCorp(corp.id, corp.name)}>
                        Unlink
                    </button>
                {/if}
            </div>

            {#if corp.wallet_divisions.length > 0}
                <div class="wallets">
                    <span class="muted small">Wallet divisions:</span>
                    {#each corp.wallet_divisions as wd (wd.division)}
                        <span class="wallet-chip">
                            Div {wd.division}: {fmtIsk(wd.balance_isk)}
                        </span>
                    {/each}
                </div>
            {:else}
                <p class="muted small">Wallet not yet synced.</p>
            {/if}

            <a href={`/groups/${groupId}/corps/${corp.id}/journal`} class="journal-link">
                View journal →
            </a>

            <div class="ambassadors">
                <span class="muted small">Ambassadors:</span>
                {#each corp.ambassadors as amb (amb.character_id)}
                    <span class="amb-chip" class:error-chip={amb.last_auth_error_at != null}>
                        {amb.character_name}
                        {#if amb.last_auth_error_at}
                            <span title="Auth error at {new Date(amb.last_auth_error_at).toLocaleString()}">⚠</span>
                        {/if}
                        {#if data.role === 'owner'}
                            <button
                                class="remove-btn"
                                onclick={() => removeAmbassador(corp.id, amb.character_id, amb.character_name)}
                                title="Remove ambassador"
                            >×</button>
                        {/if}
                    </span>
                {/each}
                {#if corp.ambassadors.length === 0}
                    <span class="muted small">None — add at least one to sync wallets.</span>
                {/if}
            </div>

            {#if data.role === 'owner'}
                {#if addingAmbFor === corp.id}
                    <div class="add-amb-row">
                        <input
                            type="number"
                            placeholder="EVE character ID"
                            bind:value={addAmbCharId}
                            style="width: 14rem"
                        />
                        <button class="primary small" onclick={() => addAmbassador(corp.id)}>
                            Add
                        </button>
                        <button class="small" onclick={() => { addingAmbFor = null; addAmbErr = null; }}>
                            Cancel
                        </button>
                        {#if addAmbErr}<span class="err small">{addAmbErr}</span>{/if}
                    </div>
                {:else}
                    <button class="small" onclick={() => { addingAmbFor = corp.id; addAmbCharId = ''; addAmbErr = null; }}>
                        + Add ambassador
                    </button>
                {/if}
            {/if}
        </section>
    {/each}

    {#if data.role === 'owner'}
        <section>
            <h2>Link a corp</h2>
            <p class="muted small">
                Enter the EVE character ID of a corp member whose affiliation will identify the corporation.
            </p>
            <div class="row">
                <input
                    type="number"
                    placeholder="EVE character ID"
                    bind:value={linkCharId}
                    style="width: 14rem"
                />
                <button class="primary" disabled={linking} onclick={linkCorp}>
                    {linking ? 'Linking…' : 'Link corp'}
                </button>
            </div>
            {#if linkErr}<p class="err">{linkErr}</p>{/if}
        </section>
    {/if}
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    section {
        margin-top: 1.25rem;
    }
    .corp-card {
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 8px;
        padding: 1rem;
        display: flex;
        flex-direction: column;
        gap: 0.6rem;
        margin-bottom: 1rem;
    }
    .corp-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }
    .ticker {
        color: #8b949e;
        margin-left: 0.35rem;
    }
    .wallets {
        display: flex;
        flex-wrap: wrap;
        gap: 0.4rem;
        align-items: center;
    }
    .wallet-chip {
        background: #21262d;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.15rem 0.55rem;
        font-size: 0.85rem;
    }
    .ambassadors {
        display: flex;
        flex-wrap: wrap;
        gap: 0.4rem;
        align-items: center;
    }
    .amb-chip {
        background: #21262d;
        border: 1px solid #30363d;
        border-radius: 999px;
        padding: 0.15rem 0.55rem;
        font-size: 0.85rem;
        display: flex;
        align-items: center;
        gap: 0.25rem;
    }
    .amb-chip.error-chip {
        border-color: #6e2832;
        color: #f87171;
    }
    .remove-btn {
        background: none;
        border: none;
        color: #8b949e;
        cursor: pointer;
        padding: 0;
        font-size: 1rem;
        line-height: 1;
    }
    .remove-btn:hover {
        color: #f87171;
    }
    .add-amb-row {
        display: flex;
        gap: 0.5rem;
        align-items: center;
        flex-wrap: wrap;
    }
    .journal-link {
        font-size: 0.9rem;
        color: #79c0ff;
    }
    .row {
        display: flex;
        gap: 0.5rem;
        align-items: center;
        margin-top: 0.5rem;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.35rem 0.75rem;
        border-radius: 6px;
        cursor: pointer;
    }
    button.primary {
        border-color: #2f6feb;
    }
    button.danger {
        border-color: #6e2832;
        color: #f87171;
    }
    button.small {
        padding: 0.2rem 0.55rem;
        font-size: 0.82rem;
    }
    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    input[type='number'] {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
    }
    .muted {
        color: #8b949e;
    }
    .small {
        font-size: 0.82rem;
    }
    .err {
        color: #f87171;
    }
</style>
