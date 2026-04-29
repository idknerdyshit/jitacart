<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import { api, fmtIsk, type CorpJournalEntry } from '$lib/api';

    const groupId = $derived(page.params.id);
    const corpId = $derived(page.params.corp_id);

    let entries = $state<CorpJournalEntry[]>([]);
    let error = $state<string | null>(null);
    let filterDivision = $state<string>('');

    async function load() {
        error = null;
        try {
            const q = filterDivision ? `?division=${filterDivision}` : '';
            entries = await api<CorpJournalEntry[]>(`/groups/${groupId}/corps/${corpId}/journal${q}`);
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(load);

    function fmtAmount(v: string): string {
        const n = Number(v);
        if (!isFinite(n)) return v;
        const sign = n >= 0 ? '+' : '';
        return sign + n.toLocaleString('en-US', { maximumFractionDigits: 2 }) + ' ISK';
    }

    function amountClass(v: string): string {
        const n = Number(v);
        if (n > 0) return 'pos';
        if (n < 0) return 'neg';
        return '';
    }
</script>

<p><a href={`/groups/${groupId}/corps`}>← Corp wallets</a></p>

<h1>Corp journal</h1>

{#if error}
    <p class="err">{error}</p>
{/if}

<div class="filter-row">
    <label for="div-filter" class="muted">Division:</label>
    <select id="div-filter" bind:value={filterDivision} onchange={load}>
        <option value="">All</option>
        {#each [1,2,3,4,5,6,7] as d}
            <option value={String(d)}>{d}</option>
        {/each}
    </select>
</div>

{#if entries.length === 0 && !error}
    <p class="muted">No journal entries yet.</p>
{:else}
    <table>
        <thead>
            <tr>
                <th>Date</th>
                <th>Div</th>
                <th>Type</th>
                <th>Amount</th>
                <th>Balance</th>
                <th>Context</th>
                <th>Reason</th>
            </tr>
        </thead>
        <tbody>
            {#each entries as e (e.id)}
                <tr>
                    <td class="muted small">{new Date(e.date).toLocaleString()}</td>
                    <td>{e.division}</td>
                    <td class="ref-type">{e.ref_type.replace(/_/g, ' ')}</td>
                    <td class={amountClass(e.amount)}>{fmtAmount(e.amount)}</td>
                    <td>{fmtIsk(e.balance)}</td>
                    <td class="muted small">
                        {#if e.context_id != null}
                            {e.context_id_type?.replace(/_/g, ' ') ?? ''} #{e.context_id}
                        {/if}
                    </td>
                    <td class="muted small">{e.reason ?? ''}</td>
                </tr>
            {/each}
        </tbody>
    </table>
{/if}

<style>
    .filter-row {
        display: flex;
        gap: 0.5rem;
        align-items: center;
        margin-bottom: 1rem;
    }
    table {
        width: 100%;
        border-collapse: collapse;
        font-size: 0.9rem;
    }
    th, td {
        text-align: left;
        padding: 0.3rem 0.5rem;
        border-bottom: 1px solid #21262d;
    }
    .ref-type {
        text-transform: capitalize;
    }
    .pos { color: #3fb950; }
    .neg { color: #f87171; }
    .muted { color: #8b949e; }
    .small { font-size: 0.82rem; }
    .err { color: #f87171; }
    select {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
    }
</style>
