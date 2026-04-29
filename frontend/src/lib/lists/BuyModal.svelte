<script lang="ts">
    import { api, findViewerClaim } from '$lib/api';
    import type { ListDetail, ListItem } from '$lib/api';

    interface Props {
        item: ListItem;
        detail: ListDetail;
        onUpdate: (d: ListDetail) => void;
        onClose: () => void;
    }

    const { item, detail, onUpdate, onClose }: Props = $props();

    // Characters for the viewer (inferred from detail: we don't have a full character list here,
    // so we only offer the last-used one as default and an "other character id" free input)
    const remaining = $derived(item.qty_requested - item.qty_fulfilled);

    let qty = $state(remaining);
    let unitPrice = $state('');
    let selectedMarketId = $state<string | null>(null);
    let otherNote = $state('');
    let useOther = $state(false);
    let charId = $state<string>(detail.last_hauler_character_id ?? '');

    let busy = $state(false);
    let errMsg = $state<string | null>(null);

    const viewerClaim = $derived(findViewerClaim(detail));

    const canSubmit = $derived(
        qty > 0 &&
            qty <= remaining &&
            unitPrice.trim() !== '' &&
            Number(unitPrice) >= 0 &&
            (useOther ? otherNote.trim() !== '' : selectedMarketId !== null)
    );

    async function submit() {
        if (!canSubmit || busy) return;
        errMsg = null;
        busy = true;
        try {
            const body: Record<string, unknown> = {
                qty,
                unit_price_isk: unitPrice,
                bought_at_market_id: useOther ? null : selectedMarketId,
                bought_at_note: useOther ? otherNote.trim() : null,
                hauler_character_id: charId.trim() || null,
                claim_id: viewerClaim?.id ?? null
            };
            const updated = await api<ListDetail>(
                `/lists/${item.list_id}/items/${item.id}/fulfillments`,
                {
                    method: 'POST',
                    headers: { 'content-type': 'application/json' },
                    body: JSON.stringify(body)
                }
            );
            onUpdate(updated);
            onClose();
        } catch (e) {
            errMsg = (e as Error).message;
        } finally {
            busy = false;
        }
    }

    function selectMarket(id: string) {
        useOther = false;
        selectedMarketId = id;
    }

    function selectOther() {
        useOther = true;
        selectedMarketId = null;
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="backdrop" onclick={onClose}>
    <div class="modal" onclick={(e) => e.stopPropagation()}>
        <h2>Record buy: {item.type_name}</h2>
        <p class="muted">Remaining: {remaining.toLocaleString()} × {item.type_name}</p>

        {#if errMsg}
            <p class="err">{errMsg}</p>
        {/if}

        <label>
            Qty bought
            <input type="number" min="1" max={remaining} bind:value={qty} />
        </label>

        <label>
            Unit price (ISK)
            <input type="number" min="0" step="0.01" bind:value={unitPrice} placeholder="0.00" />
        </label>

        <div class="field">
            <span class="label">Market</span>
            <div class="chips">
                {#each detail.markets as m (m.id)}
                    <button
                        class="chip"
                        class:selected={!useOther && selectedMarketId === m.id}
                        onclick={() => selectMarket(m.id)}
                        type="button"
                    >
                        {#if m.id === detail.primary_market_id}★ {/if}{m.short_label ?? '(unnamed)'}
                    </button>
                {/each}
                <button
                    class="chip"
                    class:selected={useOther}
                    onclick={selectOther}
                    type="button"
                >
                    Other
                </button>
            </div>
            {#if useOther}
                <p class="warn">
                    ⚠ Buying outside accepted markets — the requester may not be expecting this
                    source.
                </p>
                <input
                    type="text"
                    placeholder="Describe where you bought (required)"
                    bind:value={otherNote}
                />
            {/if}
        </div>

        <label>
            Your character ID (optional)
            <input
                type="text"
                placeholder="{detail.last_hauler_character_id ?? 'no default set'}"
                bind:value={charId}
            />
        </label>

        <div class="actions">
            <button class="primary" disabled={!canSubmit || busy} onclick={submit}>
                {busy ? 'Saving…' : 'Record buy'}
            </button>
            <button onclick={onClose} type="button">Cancel</button>
        </div>
    </div>
</div>

<style>
    .backdrop {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.6);
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 100;
    }
    .modal {
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 8px;
        padding: 1.5rem;
        min-width: 360px;
        max-width: 520px;
        width: 90vw;
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
    }
    h2 {
        margin: 0 0 0.25rem;
        font-size: 1.1rem;
    }
    label {
        display: flex;
        flex-direction: column;
        gap: 0.3rem;
        font-size: 0.9rem;
        color: #8b949e;
    }
    .field {
        display: flex;
        flex-direction: column;
        gap: 0.35rem;
    }
    .label {
        font-size: 0.9rem;
        color: #8b949e;
    }
    .chips {
        display: flex;
        flex-wrap: wrap;
        gap: 0.35rem;
    }
    .chip {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.25rem 0.65rem;
        border-radius: 999px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    .chip.selected {
        border-color: #2f6feb;
        background: #1f2937;
    }
    .actions {
        display: flex;
        gap: 0.5rem;
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
    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    input[type='text'],
    input[type='number'] {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
    }
    .muted {
        color: #8b949e;
        margin: 0;
    }
    .err {
        color: #f87171;
        margin: 0;
    }
    .warn {
        color: #fbbf24;
        font-size: 0.85rem;
        margin: 0;
    }
</style>
