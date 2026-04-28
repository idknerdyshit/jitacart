<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import { api, fmtIsk, type ListSummary } from '$lib/api';

    const groupId = $derived(page.params.id);

    let lists = $state<ListSummary[] | null>(null);
    let error = $state<string | null>(null);

    async function load() {
        error = null;
        try {
            lists = await api<ListSummary[]>(`/groups/${groupId}/lists`);
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(load);
</script>

<p><a href="/groups/{groupId}">← Group</a></p>

<h1>Lists</h1>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

<p class="actions">
    <a class="button primary" href="/groups/{groupId}/lists/new">+ New list</a>
</p>

{#if lists}
    {#if lists.length === 0}
        <p class="muted">No lists in this group yet.</p>
    {:else}
        <table>
            <thead>
                <tr>
                    <th>Destination</th>
                    <th>Hub</th>
                    <th>Items</th>
                    <th>Estimate</th>
                    <th>Status</th>
                    <th>Created</th>
                </tr>
            </thead>
            <tbody>
                {#each lists as l (l.id)}
                    <tr onclick={() => (window.location.href = `/lists/${l.id}`)}>
                        <td>{l.destination_label ?? '—'}</td>
                        <td>{l.primary_market_short_label ?? '—'}</td>
                        <td>{l.item_count}</td>
                        <td>{fmtIsk(l.total_estimate_isk)}</td>
                        <td>{l.status}</td>
                        <td class="muted">{new Date(l.created_at).toLocaleDateString()}</td>
                    </tr>
                {/each}
            </tbody>
        </table>
    {/if}
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    .actions {
        margin-top: 0.5rem;
        display: flex;
        gap: 0.5rem;
        align-items: center;
    }
    .button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.35rem 0.75rem;
        border-radius: 6px;
        cursor: pointer;
        text-decoration: none;
        display: inline-block;
    }
    .button.primary {
        border-color: #2f6feb;
    }
    table {
        width: 100%;
        border-collapse: collapse;
        margin-top: 0.5rem;
    }
    th,
    td {
        text-align: left;
        padding: 0.45rem 0.6rem;
        border-bottom: 1px solid #21262d;
    }
    tbody tr {
        cursor: pointer;
    }
    tbody tr:hover {
        background: #161b22;
    }
    .muted {
        color: #8b949e;
    }
</style>
