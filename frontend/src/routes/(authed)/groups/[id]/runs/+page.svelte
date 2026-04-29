<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import {
        api,
        fmtIsk,
        type RunSummary,
        type ContractSuggestion
    } from '$lib/api';

    const groupId = $derived(page.params.id);

    let runs = $state<RunSummary[] | null>(null);
    let error = $state<string | null>(null);
    let pendingContracts = $state<number>(0);

    async function load() {
        error = null;
        try {
            runs = await api<RunSummary[]>(`/groups/${groupId}/runs`);
        } catch (e) {
            error = (e as Error).message;
        }
        try {
            const s = await api<ContractSuggestion[]>(
                `/groups/${groupId}/contracts/suggestions`
            );
            pendingContracts = s.filter((x) => x.state === 'pending').length;
        } catch {
            // non-fatal
        }
    }

    onMount(load);

    function statusCounts(r: RunSummary): string {
        return [
            r.items_open > 0 ? `${r.items_open} open` : '',
            r.items_claimed > 0 ? `${r.items_claimed} claimed` : '',
            r.items_bought > 0 ? `${r.items_bought} bought` : '',
            r.items_delivered > 0 ? `${r.items_delivered} delivered` : '',
            r.items_settled > 0 ? `${r.items_settled} settled` : ''
        ]
            .filter(Boolean)
            .join(' · ');
    }
</script>

<p><a href={`/groups/${groupId}`}>← Group</a></p>
<div class="header">
    <h1>Available runs</h1>
    {#if pendingContracts > 0}
        <a class="contracts-chip" href={`/groups/${groupId}/contracts`}>
            {pendingContracts} pending contract match{pendingContracts === 1 ? '' : 'es'}
        </a>
    {/if}
</div>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

{#if runs !== null}
    {#if runs.length === 0}
        <p class="muted">No open lists at the moment.</p>
    {:else}
        <table>
            <thead>
                <tr>
                    <th>Destination</th>
                    <th>Markets</th>
                    <th>Items</th>
                    <th>Budget</th>
                    <th></th>
                </tr>
            </thead>
            <tbody>
                {#each runs as r (r.list_id)}
                    <tr>
                        <td>
                            <a href={`/lists/${r.list_id}`}>
                                {r.destination_label ?? '(unnamed)'}
                            </a>
                        </td>
                        <td>
                            <div class="market-chips">
                                {#each r.accepted_markets as m (m.market_id)}
                                    <span class="chip" class:primary={m.is_primary}>
                                        {#if m.is_primary}★ {/if}{m.short_label ?? '(unnamed)'}
                                    </span>
                                {/each}
                            </div>
                        </td>
                        <td class="counts">
                            {statusCounts(r)}
                        </td>
                        <td class="muted">{fmtIsk(r.total_estimate_isk)}</td>
                        <td>
                            <a
                                href={`/lists/${r.list_id}#claim`}
                                class="btn"
                                class:btn-active={r.claimed_by_me}
                            >
                                {r.claimed_by_me ? 'Continue' : 'Claim items'}
                            </a>
                        </td>
                    </tr>
                {/each}
            </tbody>
        </table>
    {/if}
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    table {
        width: 100%;
        border-collapse: collapse;
        margin-top: 1rem;
    }
    th,
    td {
        text-align: left;
        padding: 0.4rem 0.6rem;
        border-bottom: 1px solid #21262d;
    }
    .market-chips {
        display: flex;
        gap: 0.3rem;
        flex-wrap: wrap;
    }
    .chip {
        font-size: 0.78rem;
        padding: 0.1rem 0.45rem;
        border-radius: 999px;
        border: 1px solid #30363d;
        background: #21262d;
        color: #8b949e;
        white-space: nowrap;
    }
    .chip.primary {
        border-color: #2f6feb;
        color: #79c0ff;
    }
    .counts {
        font-size: 0.85rem;
        color: #8b949e;
        white-space: nowrap;
    }
    .btn {
        display: inline-block;
        padding: 0.3rem 0.7rem;
        border-radius: 6px;
        border: 1px solid #30363d;
        background: #21262d;
        color: #e6edf3;
        text-decoration: none;
        font-size: 0.85rem;
    }
    .btn.btn-active {
        border-color: #2f6feb;
        color: #79c0ff;
    }
    .muted {
        color: #8b949e;
    }
    .header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 1rem;
    }
    .contracts-chip {
        background: #1f2937;
        border: 1px solid #2f6feb;
        color: #79c0ff;
        padding: 0.3rem 0.7rem;
        border-radius: 999px;
        text-decoration: none;
        font-size: 0.85rem;
    }
</style>
