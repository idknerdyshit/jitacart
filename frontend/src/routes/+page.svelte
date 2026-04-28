<script lang="ts">
    import { onMount } from 'svelte';

    let health = $state<{ status?: string; db?: boolean } | null>(null);
    let healthError = $state<string | null>(null);

    onMount(async () => {
        try {
            const res = await fetch('/api/healthz');
            if (!res.ok) throw new Error(`HTTP ${res.status}`);
            health = await res.json();
        } catch (e) {
            healthError = e instanceof Error ? e.message : String(e);
        }
    });
</script>

<h1>JitaCart</h1>
<p>Wormhole logistics, contract-settled. Phase 0 scaffold.</p>

<section>
    <h2>API status</h2>
    {#if health}
        <pre>{JSON.stringify(health, null, 2)}</pre>
    {:else if healthError}
        <p style="color: #f87171">API unreachable: {healthError}</p>
    {:else}
        <p>checking…</p>
    {/if}
</section>

<style>
    h1 {
        margin-bottom: 0.25rem;
    }
    pre {
        background: #161b22;
        padding: 0.75rem 1rem;
        border-radius: 6px;
    }
</style>
