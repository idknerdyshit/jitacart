<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { page } from '$app/state';
    import {
        api,
        fmtIsk,
        type Market,
        type PreviewResponse,
        type ListDetail
    } from '$lib/api';

    const groupId = $derived(page.params.id);

    let markets = $state<Market[] | null>(null);
    let selectedIds = $state<Set<string>>(new Set());
    let primaryId = $state<string | null>(null);
    let multibuy = $state<string>('');
    let destinationLabel = $state<string>('');
    let notes = $state<string>('');
    let preview = $state<PreviewResponse | null>(null);
    let previewing = $state<boolean>(false);
    let error = $state<string | null>(null);
    let saving = $state<boolean>(false);

    let debounceTimer: ReturnType<typeof setTimeout> | null = null;

    onMount(async () => {
        try {
            markets = await api<Market[]>('/markets');
            // Default-select Jita as primary if present.
            const jita = markets.find((m) => m.short_label === 'Jita');
            if (jita) {
                selectedIds = new Set([jita.id]);
                primaryId = jita.id;
            }
        } catch (e) {
            error = (e as Error).message;
        }
    });

    function toggleMarket(id: string) {
        const next = new Set(selectedIds);
        if (next.has(id)) {
            next.delete(id);
            if (primaryId === id) primaryId = next.values().next().value ?? null;
        } else {
            next.add(id);
            if (primaryId == null) primaryId = id;
        }
        selectedIds = next;
        schedulePreview();
    }

    function setPrimary(id: string) {
        if (!selectedIds.has(id)) return;
        primaryId = id;
    }

    function schedulePreview() {
        if (debounceTimer) clearTimeout(debounceTimer);
        debounceTimer = setTimeout(runPreview, 300);
    }

    async function runPreview() {
        if (!multibuy.trim() || selectedIds.size === 0) {
            preview = null;
            return;
        }
        previewing = true;
        error = null;
        try {
            preview = await api<PreviewResponse>(`/groups/${groupId}/lists/preview`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ multibuy, market_ids: [...selectedIds] })
            });
        } catch (e) {
            error = (e as Error).message;
        } finally {
            previewing = false;
        }
    }

    const cheapestPerLine = $derived.by(() => {
        if (!preview) return new Map<number, string>();
        const out = new Map<number, string>();
        preview.lines.forEach((line, i) => {
            let bestId: string | null = null;
            let bestPrice: number | null = null;
            for (const [marketId, p] of Object.entries(line.prices)) {
                if (p.best_sell == null) continue;
                const n = Number(p.best_sell);
                if (bestPrice == null || n < bestPrice) {
                    bestPrice = n;
                    bestId = marketId;
                }
            }
            if (bestId) out.set(i, bestId);
        });
        return out;
    });

    const blockingErrors = $derived(
        preview != null &&
            (preview.unresolved_names.length > 0 ||
                preview.errors.length > 0 ||
                preview.lines.some((l) => l.error != null))
    );

    const canSave = $derived(
        !!preview &&
            preview.lines.length > 0 &&
            !blockingErrors &&
            !!primaryId &&
            selectedIds.size > 0 &&
            !saving
    );

    async function save() {
        if (!canSave || !primaryId) return;
        saving = true;
        error = null;
        try {
            const detail = await api<ListDetail>(`/groups/${groupId}/lists`, {
                method: 'POST',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({
                    destination_label: destinationLabel || null,
                    notes: notes || null,
                    market_ids: [...selectedIds],
                    primary_market_id: primaryId,
                    multibuy
                })
            });
            goto(`/lists/${detail.list.id}`);
        } catch (e) {
            error = (e as Error).message;
        } finally {
            saving = false;
        }
    }
</script>

<p><a href="/groups/{groupId}/lists">← Lists</a></p>

<h1>New list</h1>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

<section>
    <h2>Markets</h2>
    {#if markets}
        <div class="chips">
            {#each markets as m (m.id)}
                <button
                    class="chip"
                    class:selected={selectedIds.has(m.id)}
                    onclick={() => toggleMarket(m.id)}
                    type="button"
                >
                    {m.short_label}
                </button>
            {/each}
        </div>
        {#if selectedIds.size > 0}
            <p class="muted">Primary (used for snapshot fallback display):</p>
            <div class="chips">
                {#each markets.filter((m) => selectedIds.has(m.id)) as m (m.id)}
                    <button
                        class="chip"
                        class:selected={primaryId === m.id}
                        onclick={() => setPrimary(m.id)}
                        type="button"
                    >
                        ★ {m.short_label}
                    </button>
                {/each}
            </div>
        {/if}
    {:else}
        <p class="muted">Loading markets…</p>
    {/if}
</section>

<section>
    <h2>Multibuy paste</h2>
    <textarea
        rows="10"
        placeholder={'Tritanium\t1000\nPyerite\t500'}
        bind:value={multibuy}
        oninput={schedulePreview}
    ></textarea>
</section>

<section class="grid2">
    <label>
        <span class="muted">Destination label</span>
        <input type="text" bind:value={destinationLabel} placeholder="e.g. J123456" />
    </label>
    <label>
        <span class="muted">Notes</span>
        <input type="text" bind:value={notes} placeholder="optional" />
    </label>
</section>

{#if previewing}
    <p class="muted">Pricing…</p>
{/if}

{#if preview}
    {#if preview.errors.length > 0}
        <section>
            <h3>Parse errors</h3>
            <ul>
                {#each preview.errors as e (e.line_no)}
                    <li class="err">line {e.line_no}: {e.reason} — <code>{e.raw}</code></li>
                {/each}
            </ul>
        </section>
    {/if}
    {#if preview.unresolved_names.length > 0}
        <section>
            <h3>Unknown items</h3>
            <ul>
                {#each preview.unresolved_names as n (n)}
                    <li class="err">{n}</li>
                {/each}
            </ul>
        </section>
    {/if}

    <section>
        <h2>Preview</h2>
        <table>
            <thead>
                <tr>
                    <th>Item</th>
                    <th>Qty</th>
                    {#each markets ?? [] as m (m.id)}
                        {#if selectedIds.has(m.id)}
                            <th>{m.short_label}</th>
                        {/if}
                    {/each}
                </tr>
            </thead>
            <tbody>
                {#each preview.lines as line, i (line.name + i)}
                    <tr class:err-row={line.error != null}>
                        <td>
                            {#if line.error}<span class="err">⚠ </span>{/if}
                            {line.type_name ?? line.name}
                        </td>
                        <td>{line.qty.toLocaleString()}</td>
                        {#each markets ?? [] as m (m.id)}
                            {#if selectedIds.has(m.id)}
                                <td class:cheapest={cheapestPerLine.get(i) === m.id}>
                                    {fmtIsk(line.prices[m.id]?.best_sell ?? null)}
                                </td>
                            {/if}
                        {/each}
                    </tr>
                {/each}
            </tbody>
        </table>
    </section>
{/if}

<section>
    <button class="primary" disabled={!canSave} onclick={save}>
        {saving ? 'Saving…' : 'Save list'}
    </button>
</section>

<style>
    section {
        margin-top: 1.25rem;
    }
    h2,
    h3 {
        margin-bottom: 0.5rem;
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
    textarea,
    input[type='text'] {
        width: 100%;
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.5rem 0.6rem;
        font-family: ui-monospace, Menlo, monospace;
    }
    button.primary {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #2f6feb;
        padding: 0.45rem 1rem;
        border-radius: 6px;
        cursor: pointer;
    }
    button.primary:disabled {
        opacity: 0.5;
        cursor: not-allowed;
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
    .cheapest {
        background: #1d3a2a;
        font-weight: 600;
    }
    .err {
        color: #f87171;
    }
    .err-row td {
        background: rgba(248, 113, 113, 0.05);
    }
    .muted {
        color: #8b949e;
    }
    .grid2 {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 0.75rem;
    }
    code {
        background: #161b22;
        padding: 0.1rem 0.4rem;
        border-radius: 4px;
    }
</style>
