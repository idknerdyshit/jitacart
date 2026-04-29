<script lang="ts">
    import { api, fmtIsk, fmtPct } from '$lib/api';
    import type { ListDetail, Reimbursement } from '$lib/api';

    interface Props {
        detail: ListDetail;
        onUpdate: (d: ListDetail) => void;
    }

    const { detail, onUpdate }: Props = $props();

    let settling = $state<string | null>(null);
    let errMsg = $state<string | null>(null);

    const visible = $derived(detail.reimbursements.filter((r) => Number(r.total_isk) > 0 || r.status !== 'pending'));

    function canSettle(r: Reimbursement): boolean {
        const isRequester = detail.viewer_user_id === r.requester_user_id;
        const isOwner = detail.viewer_role === 'owner';
        return (isRequester || isOwner) && r.status === 'pending';
    }

    function hasUndelivered(r: Reimbursement): boolean {
        return detail.items.some(
            (it) =>
                it.requested_by_user_id === r.requester_user_id &&
                it.status !== 'delivered' &&
                it.status !== 'settled' &&
                detail.fulfillments.some(
                    (f) =>
                        f.list_item_id === it.id &&
                        f.hauler_user_id === r.hauler_user_id &&
                        f.reversed_at === null
                )
        );
    }

    async function settle(r: Reimbursement) {
        if (settling) return;
        errMsg = null;
        settling = r.id;
        try {
            const updated = await api<ListDetail>(`/reimbursements/${r.id}/settle`, {
                method: 'POST'
            });
            onUpdate(updated);
        } catch (e) {
            errMsg = (e as Error).message;
        } finally {
            settling = null;
        }
    }
</script>

{#if visible.length > 0}
    <section>
        <h2>Reimbursements</h2>
        {#if errMsg}
            <p class="err">{errMsg}</p>
        {/if}
        <div class="cards">
            {#each visible as r (r.id)}
                <div class="card" class:settled={r.status === 'settled'}>
                    <div class="row-between">
                        <span class="parties">
                            <strong>{r.requester_display_name}</strong>
                            <span class="arrow">→</span>
                            <strong>{r.hauler_display_name}</strong>
                        </span>
                        <span class="pill" class:pill-settled={r.status === 'settled'} class:pill-cancelled={r.status === 'cancelled'}>
                            {r.status}
                        </span>
                    </div>
                    <div class="amounts">
                        <span class="muted">Subtotal</span>
                        <span>{fmtIsk(r.subtotal_isk)}</span>
                        <span class="muted">Tip ({fmtPct(detail.list.tip_pct)})</span>
                        <span>{fmtIsk(r.tip_isk)}</span>
                        <span class="muted">Total</span>
                        <span class="total">{fmtIsk(r.total_isk)}</span>
                    </div>
                    {#if r.status === 'settled' && r.settled_at}
                        <p class="muted small">
                            Settled {new Date(r.settled_at).toLocaleString()}
                        </p>
                    {/if}
                    {#if canSettle(r)}
                        {#if hasUndelivered(r)}
                            <p class="warn small">All items from this hauler must be fully bought and delivered before settling.</p>
                        {/if}
                        <button
                            class="primary"
                            disabled={settling === r.id || hasUndelivered(r)}
                            onclick={() => settle(r)}
                        >
                            {settling === r.id ? 'Settling…' : 'Mark settled'}
                        </button>
                    {/if}
                </div>
            {/each}
        </div>
    </section>
{/if}

<style>
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
    .card.settled {
        opacity: 0.7;
    }
    .row-between {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }
    .parties {
        display: flex;
        gap: 0.4rem;
        align-items: center;
    }
    .arrow {
        color: #8b949e;
    }
    .amounts {
        display: grid;
        grid-template-columns: auto auto;
        gap: 0.15rem 1rem;
        font-size: 0.9rem;
    }
    .total {
        font-weight: 600;
    }
    .pill {
        font-size: 0.75rem;
        padding: 0.15rem 0.5rem;
        border-radius: 999px;
        background: #21262d;
        border: 1px solid #388bfd;
        color: #79c0ff;
    }
    .pill-settled {
        border-color: #3fb950;
        color: #3fb950;
    }
    .pill-cancelled {
        border-color: #6e2832;
        color: #f87171;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.35rem 0.75rem;
        border-radius: 6px;
        cursor: pointer;
        align-self: flex-start;
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
    .small {
        font-size: 0.8rem;
        margin: 0;
    }
    .err {
        color: #f87171;
    }
    .warn {
        color: #fbbf24;
        margin: 0;
    }
</style>
