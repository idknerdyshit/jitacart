<script lang="ts">
    interface Props {
        open: boolean;
        title: string;
        message: string;
        confirmLabel?: string;
        cancelLabel?: string;
        onConfirm: () => void;
        onCancel: () => void;
    }

    const {
        open,
        title,
        message,
        confirmLabel = 'Delete',
        cancelLabel = 'Cancel',
        onConfirm,
        onCancel
    }: Props = $props();

    let confirmBtnEl = $state<HTMLButtonElement | null>(null);

    function onModalMount(node: HTMLDivElement) {
        queueMicrotask(() => confirmBtnEl?.focus());
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                e.stopPropagation();
                onCancel();
            }
        };
        node.addEventListener('keydown', onKey);
        return {
            destroy: () => node.removeEventListener('keydown', onKey)
        };
    }
</script>

{#if open}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="backdrop" onclick={onCancel}>
        <div
            class="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="delete-confirm-title"
            tabindex="-1"
            use:onModalMount
            onclick={(e) => e.stopPropagation()}
        >
            <h2 id="delete-confirm-title">{title}</h2>
            <p>{message}</p>
            <div class="actions">
                <button
                    class="danger"
                    onclick={onConfirm}
                    bind:this={confirmBtnEl}
                    type="button"
                >
                    {confirmLabel}
                </button>
                <button onclick={onCancel} type="button">{cancelLabel}</button>
            </div>
        </div>
    </div>
{/if}

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
        min-width: 320px;
        max-width: 480px;
        width: 90vw;
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
    }
    h2 {
        margin: 0 0 0.25rem;
        font-size: 1.1rem;
    }
    p {
        margin: 0;
        color: #c9d1d9;
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
    button.danger {
        border-color: #f87171;
        color: #f87171;
    }
</style>
