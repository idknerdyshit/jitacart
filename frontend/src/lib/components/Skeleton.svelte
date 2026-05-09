<script lang="ts">
    interface Props {
        rows?: number;
        columns?: number;
        variant?: 'table' | 'card';
    }
    const { rows = 4, columns = 4, variant = 'table' }: Props = $props();
</script>

{#if variant === 'table'}
    <div class="skeleton-table">
        <div class="skeleton-header">
            {#each Array(columns) as _, i}
                <div class="skeleton-cell header" style="width: {60 + (i * 15) % 40}%"></div>
            {/each}
        </div>
        {#each Array(rows) as _}
            <div class="skeleton-row">
                {#each Array(columns) as _, i}
                    <div class="skeleton-cell" style="width: {40 + (i * 20) % 50}%"></div>
                {/each}
            </div>
        {/each}
    </div>
{:else}
    <div class="skeleton-cards">
        {#each Array(rows) as _}
            <div class="skeleton-card">
                <div class="skeleton-cell" style="width: 60%"></div>
                <div class="skeleton-cell short" style="width: 40%"></div>
            </div>
        {/each}
    </div>
{/if}

<style>
    .skeleton-table {
        display: flex;
        flex-direction: column;
        gap: 0;
    }
    .skeleton-header,
    .skeleton-row {
        display: flex;
        gap: 0.6rem;
        padding: 0.5rem 0.6rem;
        border-bottom: 1px solid #21262d;
    }
    .skeleton-cell {
        height: 1rem;
        background: #21262d;
        border-radius: 4px;
        animation: pulse 1.5s ease-in-out infinite;
    }
    .skeleton-cell.header {
        height: 0.75rem;
        opacity: 0.5;
    }
    .skeleton-cards {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
    }
    .skeleton-card {
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 8px;
        padding: 0.85rem 1rem;
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }
    .skeleton-cell.short {
        height: 0.75rem;
    }
    @keyframes pulse {
        0%,
        100% {
            opacity: 0.4;
        }
        50% {
            opacity: 0.8;
        }
    }
</style>
