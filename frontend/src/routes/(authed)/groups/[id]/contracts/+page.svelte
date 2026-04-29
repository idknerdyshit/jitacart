<script lang="ts">
    import { onMount } from 'svelte';
    import { page } from '$app/state';
    import {
        api,
        deltaClass,
        fmtIsk,
        isContractTerminalFailure,
        isContractTerminalSuccess,
        type ContractSuggestion,
        type BoundContract
    } from '$lib/api';

    const groupId = $derived(page.params.id);

    let suggestions = $state<ContractSuggestion[] | null>(null);
    let bound = $state<BoundContract[] | null>(null);
    let error = $state<string | null>(null);
    let busy = $state<string | null>(null);

    let tab = $state<'pending' | 'in_progress' | 'settled' | 'warnings'>('pending');

    async function load() {
        error = null;
        try {
            const [s, b] = await Promise.all([
                api<ContractSuggestion[]>(`/groups/${groupId}/contracts/suggestions`),
                api<BoundContract[]>(`/groups/${groupId}/contracts`)
            ]);
            suggestions = s;
            bound = b;
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(load);

    async function confirm(s: ContractSuggestion) {
        if (busy) return;
        busy = s.id;
        try {
            await api(`/contracts/suggestions/${s.id}/confirm`, { method: 'POST' });
            await load();
        } catch (e) {
            error = (e as Error).message;
        } finally {
            busy = null;
        }
    }

    async function reject(s: ContractSuggestion) {
        if (busy) return;
        busy = s.id;
        try {
            await api(`/contracts/suggestions/${s.id}/reject`, { method: 'POST' });
            await load();
        } catch (e) {
            error = (e as Error).message;
        } finally {
            busy = null;
        }
    }

    async function unlink(c: BoundContract) {
        if (busy) return;
        busy = c.contract_id;
        try {
            await api(`/contracts/${c.contract_id}/unlink`, { method: 'POST' });
            await load();
        } catch (e) {
            error = (e as Error).message;
        } finally {
            busy = null;
        }
    }

    const pending = $derived(
        (suggestions ?? []).filter((s) => s.state === 'pending')
    );

    const inProgress = $derived(
        (bound ?? []).filter((c) => c.status === 'outstanding' || c.status === 'in_progress')
    );
    const settled = $derived((bound ?? []).filter((c) => isContractTerminalSuccess(c.status)));
    const warnings = $derived((bound ?? []).filter((c) => isContractTerminalFailure(c.status)));
</script>

<p><a href={`/groups/${groupId}`}>← Group</a></p>
<h1>Contracts</h1>

{#if error}
    <p class="err">{error}</p>
{/if}

<div class="tabs">
    <button class:active={tab === 'pending'} onclick={() => (tab = 'pending')}>
        Pending ({pending.length})
    </button>
    <button class:active={tab === 'in_progress'} onclick={() => (tab = 'in_progress')}>
        Bound ({inProgress.length})
    </button>
    <button class:active={tab === 'settled'} onclick={() => (tab = 'settled')}>
        Settled ({settled.length})
    </button>
    <button class:active={tab === 'warnings'} onclick={() => (tab = 'warnings')}>
        Warnings ({warnings.length})
    </button>
</div>

{#if tab === 'pending'}
    {#if pending.length === 0}
        <p class="muted">No pending matches.</p>
    {:else}
        <div class="cards">
            {#each pending as s (s.id)}
                <div class="card">
                    <div class="row-between">
                        <span>
                            <strong>Contract #{s.esi_contract_id}</strong>
                            <span class="muted">→ {s.list_destination_label ?? '(unnamed)'}</span>
                        </span>
                        {#if s.exact_match}
                            <span class="pill ok">exact</span>
                        {:else}
                            <span class="pill">{Math.round(Number(s.score) * 100)}%</span>
                        {/if}
                    </div>
                    <div class="row-between small">
                        <span>{s.requester_display_name} owes {s.hauler_display_name}</span>
                        <span>{fmtIsk(s.reimbursement_total_isk)}</span>
                    </div>
                    <div class="row-between small">
                        <span class="muted">contract price</span>
                        <span>{fmtIsk(s.contract_price_isk)}</span>
                    </div>
                    <div class="actions">
                        <button
                            class="primary"
                            disabled={busy === s.id}
                            onclick={() => confirm(s)}
                        >
                            {busy === s.id ? '…' : 'Confirm match'}
                        </button>
                        <button disabled={busy === s.id} onclick={() => reject(s)}>
                            Reject
                        </button>
                    </div>
                </div>
            {/each}
        </div>
    {/if}
{:else if tab === 'in_progress'}
    {#if inProgress.length === 0}
        <p class="muted">No bound contracts in flight.</p>
    {:else}
        <div class="cards">
            {#each inProgress as c (c.contract_id)}
                <div class="card">
                    <div class="row-between">
                        <strong>Contract #{c.esi_contract_id}</strong>
                        <span class="pill">{c.status.replace('_', ' ')}</span>
                    </div>
                    <div class="row-between small">
                        <span class="muted">{c.bound_reimbursement_count} reimbursement(s)</span>
                        <span>{fmtIsk(c.price_isk)}</span>
                    </div>
                    <div class="actions">
                        <button
                            disabled={busy === c.contract_id}
                            onclick={() => unlink(c)}
                        >
                            Unlink
                        </button>
                    </div>
                </div>
            {/each}
        </div>
    {/if}
{:else if tab === 'settled'}
    {#if settled.length === 0}
        <p class="muted">No settled contracts yet.</p>
    {:else}
        <div class="cards">
            {#each settled as c (c.contract_id)}
                <div class="card">
                    <div class="row-between">
                        <strong>Contract #{c.esi_contract_id}</strong>
                        <span class="pill ok">settled</span>
                    </div>
                    <div class="amounts">
                        <span class="muted">price</span>
                        <span>{fmtIsk(c.price_isk)}</span>
                        <span class="muted">expected</span>
                        <span>{fmtIsk(c.expected_total_isk)}</span>
                        <span class="muted">delta</span>
                        <span class={deltaClass(c.settlement_delta_isk)}>
                            {fmtIsk(c.settlement_delta_isk)}
                        </span>
                    </div>
                    {#if c.date_completed}
                        <p class="muted small">
                            Finished {new Date(c.date_completed).toLocaleString()}
                        </p>
                    {/if}
                </div>
            {/each}
        </div>
    {/if}
{:else if tab === 'warnings'}
    {#if warnings.length === 0}
        <p class="muted">No warnings.</p>
    {:else}
        <div class="cards">
            {#each warnings as c (c.contract_id)}
                <div class="card warn-card">
                    <div class="row-between">
                        <strong>Contract #{c.esi_contract_id}</strong>
                        <span class="pill bad">{c.status}</span>
                    </div>
                    <p class="small muted">
                        Bound reimbursements have been returned to pending.
                    </p>
                </div>
            {/each}
        </div>
    {/if}
{/if}

<style>
    .tabs {
        display: flex;
        gap: 0.4rem;
        margin: 1rem 0;
    }
    .tabs button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.3rem 0.7rem;
        border-radius: 999px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    .tabs button.active {
        border-color: #2f6feb;
        color: #79c0ff;
    }
    .cards {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
    }
    .card {
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 8px;
        padding: 0.85rem 1rem;
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }
    .warn-card {
        border-color: #6e2832;
    }
    .row-between {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }
    .small {
        font-size: 0.85rem;
    }
    .amounts {
        display: grid;
        grid-template-columns: auto auto;
        gap: 0.15rem 1rem;
        font-size: 0.9rem;
    }
    .pill {
        font-size: 0.75rem;
        padding: 0.15rem 0.5rem;
        border-radius: 999px;
        background: #21262d;
        border: 1px solid #388bfd;
        color: #79c0ff;
    }
    .pill.ok {
        border-color: #3fb950;
        color: #3fb950;
    }
    .pill.bad {
        border-color: #6e2832;
        color: #f87171;
    }
    .actions {
        display: flex;
        gap: 0.4rem;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.3rem 0.7rem;
        border-radius: 6px;
        cursor: pointer;
    }
    button.primary {
        border-color: #3fb950;
        color: #3fb950;
    }
    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    .muted {
        color: #8b949e;
    }
    .err {
        color: #f87171;
    }
    .pos {
        color: #3fb950;
    }
    .neg {
        color: #f87171;
    }
</style>
