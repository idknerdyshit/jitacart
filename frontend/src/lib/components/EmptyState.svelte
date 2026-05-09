<script lang="ts">
    interface Props {
        message: string;
        hint?: string;
    }
    const { message, hint }: Props = $props();
</script>

<div class="empty">
    <p>{message}</p>
    {#if hint}
        <p class="hint">{hint}</p>
    {/if}
</div>

<style>
    .empty {
        text-align: center;
        padding: 2rem 1rem;
        color: #8b949e;
    }
    .empty p {
        margin: 0.25rem 0;
    }
    .hint {
        font-size: 0.85rem;
        opacity: 0.7;
    }
</style>
