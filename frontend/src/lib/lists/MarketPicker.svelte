<script lang="ts">
    import { isMarketStale, type GroupMarket } from '$lib/api';
    import { SvelteSet } from 'svelte/reactivity';

    interface Props {
        markets: GroupMarket[];
        selected: SvelteSet<string>;
        primary: string | null;
        onToggle: (id: string) => void;
        onSetPrimary: (id: string) => void;
        primaryLabel?: string;
    }
    let {
        markets,
        selected,
        primary,
        onToggle,
        onSetPrimary,
        primaryLabel = 'Primary:'
    }: Props = $props();

    function chipTitle(m: GroupMarket): string {
        if (m.kind !== 'public_structure') return m.name ?? '';
        const via = m.accessing_character_name ? ` · via ${m.accessing_character_name}` : '';
        const stale = isMarketStale(m) ? ' · stale' : '';
        return `${m.name ?? '(unnamed)'}${via}${stale}`;
    }

    const selectedMarkets = $derived(markets.filter((m) => selected.has(m.id)));
</script>

{#snippet citadelBadge(m: GroupMarket)}
    {#if m.kind === 'public_structure'}
        <span class="badge">citadel</span>
    {/if}
{/snippet}

<div class="chips">
    {#each markets as m (m.id)}
        <button
            class="chip"
            class:selected={selected.has(m.id)}
            class:stale={isMarketStale(m)}
            onclick={() => onToggle(m.id)}
            type="button"
            aria-pressed={selected.has(m.id)}
            aria-label={`Toggle ${m.short_label ?? '(unnamed)'} market`}
            title={chipTitle(m)}
        >
            {m.short_label ?? '(unnamed)'}
            {@render citadelBadge(m)}
        </button>
    {/each}
</div>

{#if selected.size > 0}
    <p class="muted">{primaryLabel}</p>
    <div class="chips">
        {#each selectedMarkets as m (m.id)}
            <button
                class="chip"
                class:selected={primary === m.id}
                onclick={() => onSetPrimary(m.id)}
                type="button"
                aria-pressed={primary === m.id}
                aria-label={`Mark ${m.short_label ?? '(unnamed)'} as primary market`}
            >
                ★ {m.short_label ?? '(unnamed)'}
                {@render citadelBadge(m)}
            </button>
        {/each}
    </div>
{/if}

<style>
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
    .muted {
        color: #8b949e;
    }
</style>
