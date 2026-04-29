<script lang="ts">
    import { api, findViewerClaim } from '$lib/api';
    import type { ListDetail, ListItem } from '$lib/api';

    interface Props {
        item: ListItem;
        detail: ListDetail;
        onUpdate: (d: ListDetail) => void;
    }

    const { item, detail, onUpdate }: Props = $props();

    let busy = $state(false);
    let errMsg = $state<string | null>(null);

    const viewerClaim = $derived(findViewerClaim(detail));

    const itemClaim = $derived(
        detail.claims.find((c) => c.item_ids.includes(item.id) && c.status === 'active') ?? null
    );

    const isMine = $derived(
        viewerClaim !== null && viewerClaim.item_ids.includes(item.id)
    );

    const isOther = $derived(
        itemClaim !== null && itemClaim.hauler_user_id !== detail.viewer_user_id
    );

    function initial(name: string): string {
        return name.trim().charAt(0).toUpperCase();
    }

    async function toggle() {
        if (busy) return;
        errMsg = null;
        busy = true;
        try {
            let updated: ListDetail;
            if (isMine && viewerClaim) {
                // Remove item from claim
                updated = await api<ListDetail>(
                    `/claims/${viewerClaim.id}/items/${item.id}`,
                    { method: 'DELETE' }
                );
            } else if (viewerClaim) {
                // Add item to existing claim
                updated = await api<ListDetail>(`/claims/${viewerClaim.id}/items`, {
                    method: 'POST',
                    headers: { 'content-type': 'application/json' },
                    body: JSON.stringify({ item_ids: [item.id] })
                });
            } else {
                // Create new claim with this item
                updated = await api<ListDetail>(`/lists/${item.list_id}/claims`, {
                    method: 'POST',
                    headers: { 'content-type': 'application/json' },
                    body: JSON.stringify({ item_ids: [item.id] })
                });
            }
            onUpdate(updated);
        } catch (e) {
            const msg = (e as Error).message;
            // On 409, reload and let parent retry
            if (msg.startsWith('409')) {
                errMsg = 'Already claimed — reloading…';
                try {
                    const refreshed = await api<ListDetail>(`/lists/${item.list_id}`);
                    onUpdate(refreshed);
                } catch {
                    // ignore secondary error
                }
            } else {
                errMsg = msg;
            }
        } finally {
            busy = false;
        }
    }
</script>

{#if errMsg}
    <span class="err" title={errMsg}>!</span>
{/if}

{#if isOther && itemClaim}
    <span class="chip other" title={`Claimed by ${itemClaim.hauler_display_name}`}>
        {initial(itemClaim.hauler_display_name)}
    </span>
{:else if isMine}
    <button class="chip mine" onclick={toggle} disabled={busy} title="Remove claim">
        mine
    </button>
{:else if item.status === 'open' || item.status === 'claimed'}
    <button class="chip add" onclick={toggle} disabled={busy} title="Claim this item">
        +
    </button>
{/if}

<style>
    .chip {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        font-size: 0.75rem;
        padding: 0.15rem 0.5rem;
        border-radius: 999px;
        border: 1px solid #30363d;
        cursor: pointer;
        background: #21262d;
        color: #e6edf3;
        white-space: nowrap;
        user-select: none;
    }
    .chip.mine {
        border-color: #2f6feb;
        color: #79c0ff;
        background: #1c2d50;
    }
    .chip.add {
        border-color: #30363d;
        color: #8b949e;
    }
    .chip.add:hover:not(:disabled) {
        border-color: #2f6feb;
        color: #79c0ff;
    }
    .chip.other {
        cursor: default;
        border-color: #388bfd;
        background: #1c2d50;
        color: #8b949e;
    }
    .chip:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    .err {
        color: #f87171;
        font-size: 0.75rem;
        margin-right: 0.25rem;
    }
</style>
