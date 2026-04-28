<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { page } from '$app/state';
    import {
        api,
        fmtIsk,
        type ListDetail,
        type ListStatus,
        type LiveItemPrice,
        type GroupMarket
    } from '$lib/api';

    const listId = $derived(page.params.id);
    const STALE_AFTER_MS = 2 * 600 * 1000;

    let detail = $state<ListDetail | null>(null);
    let allMarkets = $state<GroupMarket[] | null>(null);
    let error = $state<string | null>(null);
    let editingMarkets = $state<boolean>(false);
    let editSelected = $state<Set<string>>(new Set());
    let editPrimary = $state<string | null>(null);

    let addingItem = $state<boolean>(false);
    let newItemName = $state<string>('');
    let newItemQty = $state<number>(1);

    async function load() {
        error = null;
        try {
            const d = await api<ListDetail>(`/lists/${listId}`);
            const all = allMarkets
                ? allMarkets
                : await api<GroupMarket[]>(`/groups/${d.list.group_id}/markets`);
            detail = d;
            allMarkets = all;
            editSelected = new Set(d.markets.map((m) => m.id));
            editPrimary = d.primary_market_id;
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(load);

    async function setStatus(status: ListStatus) {
        if (!detail) return;
        try {
            detail = await api<ListDetail>(`/lists/${listId}`, {
                method: 'PATCH',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ status })
            });
        } catch (e) {
            error = (e as Error).message;
        }
    }

    async function deleteList() {
        if (!detail) return;
        if (!confirm('Delete this list?')) return;
        try {
            const groupId = detail.list.group_id;
            await api(`/lists/${listId}`, { method: 'DELETE' });
            goto(`/groups/${groupId}/lists`);
        } catch (e) {
            error = (e as Error).message;
        }
    }

    function toggleEditMarket(id: string) {
        const next = new Set(editSelected);
        if (next.has(id)) {
            next.delete(id);
            if (editPrimary === id) editPrimary = next.values().next().value ?? null;
        } else {
            next.add(id);
            if (editPrimary == null) editPrimary = id;
        }
        editSelected = next;
    }

    async function saveMarkets() {
        if (!editPrimary || editSelected.size === 0) return;
        try {
            detail = await api<ListDetail>(`/lists/${listId}/markets`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({
                    market_ids: [...editSelected],
                    primary_market_id: editPrimary
                })
            });
            editingMarkets = false;
        } catch (e) {
            error = (e as Error).message;
        }
    }

    async function addItem() {
        if (!newItemName.trim() || newItemQty <= 0) return;
        addingItem = true;
        try {
            detail = await api<ListDetail>(`/lists/${listId}/items`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ type_name: newItemName.trim(), qty: newItemQty })
            });
            newItemName = '';
            newItemQty = 1;
        } catch (e) {
            error = (e as Error).message;
        } finally {
            addingItem = false;
        }
    }

    async function deleteItem(itemId: string) {
        try {
            detail = await api<ListDetail>(`/lists/${listId}/items/${itemId}`, {
                method: 'DELETE'
            });
        } catch (e) {
            error = (e as Error).message;
        }
    }

    async function updateQty(itemId: string, qty: number) {
        if (qty <= 0) return;
        try {
            detail = await api<ListDetail>(`/lists/${listId}/items/${itemId}`, {
                method: 'PATCH',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ qty_requested: qty })
            });
        } catch (e) {
            error = (e as Error).message;
        }
    }

    function isStaleMarket(mid: string): boolean {
        const m = allMarkets?.find((x) => x.id === mid);
        if (!m) return false;
        if (!m.is_public) return true;
        if (m.kind !== 'public_structure') return false;
        if (!m.last_orders_synced_at) return true;
        return Date.now() - new Date(m.last_orders_synced_at).getTime() > STALE_AFTER_MS;
    }

    function isCitadel(mid: string): boolean {
        return allMarkets?.find((x) => x.id === mid)?.kind === 'public_structure';
    }

    const priceIndex = $derived.by(() => {
        const m = new Map<string, LiveItemPrice>();
        if (detail) {
            for (const p of detail.live_prices) {
                m.set(`${p.list_item_id}|${p.market_id}`, p);
            }
        }
        return m;
    });
    function priceFor(itemId: string, marketId: string) {
        return priceIndex.get(`${itemId}|${marketId}`) ?? null;
    }
</script>

<p>
    <a
        href={detail ? `/groups/${detail.list.group_id}/lists` : '/'}
    >← Lists</a>
</p>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

{#if detail}
    <h1>{detail.list.destination_label ?? '(unnamed list)'}</h1>
    <p class="muted">
        Status: {detail.list.status} · Saved budget:
        {fmtIsk(detail.list.total_estimate_isk)}
    </p>

    {#if detail.list.notes}
        <p>{detail.list.notes}</p>
    {/if}

    <section>
        <h2>Status</h2>
        <select
            value={detail.list.status}
            onchange={(e) => setStatus((e.currentTarget as HTMLSelectElement).value as ListStatus)}
        >
            <option value="open">open</option>
            <option value="closed">closed</option>
            <option value="archived">archived</option>
        </select>
    </section>

    <section>
        <div class="row-between">
            <h2>Markets</h2>
            <button onclick={() => (editingMarkets = !editingMarkets)}>
                {editingMarkets ? 'Cancel' : 'Edit'}
            </button>
        </div>
        {#if editingMarkets && allMarkets}
            <div class="chips">
                {#each allMarkets as m (m.id)}
                    <button
                        class="chip"
                        class:selected={editSelected.has(m.id)}
                        class:stale={isStaleMarket(m.id)}
                        onclick={() => toggleEditMarket(m.id)}
                        type="button"
                        title={m.kind === 'public_structure'
                            ? `${m.name ?? ''}${m.accessing_character_name ? ` · via ${m.accessing_character_name}` : ''}`
                            : (m.name ?? '')}
                    >
                        {m.short_label ?? '(unnamed)'}
                        {#if m.kind === 'public_structure'}
                            <span class="badge">citadel</span>
                        {/if}
                    </button>
                {/each}
            </div>
            {#if editSelected.size > 0}
                <p class="muted">Primary:</p>
                <div class="chips">
                    {#each allMarkets.filter((m) => editSelected.has(m.id)) as m (m.id)}
                        <button
                            class="chip"
                            class:selected={editPrimary === m.id}
                            onclick={() => (editPrimary = m.id)}
                            type="button"
                        >
                            ★ {m.short_label ?? '(unnamed)'}
                            {#if m.kind === 'public_structure'}
                                <span class="badge">citadel</span>
                            {/if}
                        </button>
                    {/each}
                </div>
            {/if}
            <button class="primary" onclick={saveMarkets}>Save markets</button>
        {:else}
            <p>
                {#each detail.markets as m, i (m.id)}
                    {#if i > 0}, {/if}
                    {#if m.id === detail.primary_market_id}<strong>★ {m.short_label ?? '(unnamed)'}</strong
                        >{:else}{m.short_label ?? '(unnamed)'}{/if}
                    {#if m.kind === 'public_structure'}<span class="badge">citadel</span>{/if}
                {/each}
            </p>
        {/if}
    </section>

    <section>
        <h2>Items</h2>
        <table>
            <thead>
                <tr>
                    <th>Item</th>
                    <th>Qty</th>
                    <th>Saved unit</th>
                    {#each detail.markets as m (m.id)}
                        <th>
                            {m.short_label ?? '(unnamed)'}
                            {#if m.kind === 'public_structure'}<span class="badge">citadel</span>{/if}
                        </th>
                    {/each}
                    <th></th>
                </tr>
            </thead>
            <tbody>
                {#each detail.items as it (it.id)}
                    <tr>
                        <td>{it.type_name}</td>
                        <td>
                            <input
                                type="number"
                                min="1"
                                value={it.qty_requested}
                                onchange={(e) =>
                                    updateQty(
                                        it.id,
                                        Number((e.currentTarget as HTMLInputElement).value)
                                    )}
                                style="width: 6rem"
                            />
                        </td>
                        <td>{fmtIsk(it.est_unit_price_isk)}</td>
                        {#each detail.markets as m (m.id)}
                            {@const lp = priceFor(it.id, m.id)}
                            {@const cellStale = isStaleMarket(m.id) ||
                                (lp?.computed_at && Date.now() - new Date(lp.computed_at).getTime() > STALE_AFTER_MS)}
                            <td
                                class:stale={cellStale}
                                title={lp?.computed_at == null
                                    ? 'worker has not priced this yet'
                                    : `priced at ${new Date(lp.computed_at).toLocaleTimeString()}`}
                            >
                                {#if lp == null || lp.best_sell == null}
                                    <span class="muted">no offers</span>
                                {:else}
                                    {fmtIsk(lp.best_sell)}
                                    {#if lp.sell_volume <= it.qty_requested}
                                        <span class="warn">·thin (vol {lp.sell_volume.toLocaleString()})</span>
                                    {:else}
                                        <span class="muted vol">vol {lp.sell_volume.toLocaleString()}</span>
                                    {/if}
                                    {#if cellStale}<span class="muted">·stale</span>{/if}
                                {/if}
                            </td>
                        {/each}
                        <td>
                            <button class="danger" onclick={() => deleteItem(it.id)}>×</button>
                        </td>
                    </tr>
                {/each}
            </tbody>
        </table>
    </section>

    <section>
        <h3>Add item</h3>
        <div class="row">
            <input
                type="text"
                placeholder="Type name (e.g. Tritanium)"
                bind:value={newItemName}
            />
            <input type="number" min="1" bind:value={newItemQty} style="width: 8rem" />
            <button class="primary" disabled={addingItem || !newItemName.trim()} onclick={addItem}>
                Add
            </button>
        </div>
    </section>

    <section>
        <button class="danger" onclick={deleteList}>Delete list</button>
    </section>
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    section {
        margin-top: 1.25rem;
    }
    h2,
    h3 {
        margin-bottom: 0.5rem;
    }
    .row {
        display: flex;
        gap: 0.5rem;
        align-items: center;
    }
    .row-between {
        display: flex;
        justify-content: space-between;
        align-items: center;
    }
    .chips {
        display: flex;
        gap: 0.4rem;
        flex-wrap: wrap;
        margin-bottom: 0.5rem;
    }
    .chip {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.3rem 0.7rem;
        border-radius: 999px;
        cursor: pointer;
    }
    .chip.selected {
        border-color: #2f6feb;
        background: #1f2937;
    }
    .chip.stale {
        opacity: 0.5;
    }
    .badge {
        font-size: 0.7em;
        padding: 0.05em 0.4em;
        border-radius: 4px;
        background: #30363d;
        color: #8b949e;
        margin-left: 0.35em;
        text-transform: uppercase;
        letter-spacing: 0.04em;
    }
    td.stale {
        opacity: 0.55;
    }
    .vol {
        font-size: 0.85em;
        margin-left: 0.4em;
    }
    .warn {
        color: #fbbf24;
        font-size: 0.85em;
        margin-left: 0.4em;
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
    table {
        width: 100%;
        border-collapse: collapse;
    }
    th,
    td {
        text-align: left;
        padding: 0.35rem 0.55rem;
        border-bottom: 1px solid #21262d;
    }
    input[type='text'],
    input[type='number'],
    select {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
    }
    .muted {
        color: #8b949e;
    }
</style>
