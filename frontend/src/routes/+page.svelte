<script lang="ts">
    import { onMount } from 'svelte';

    type Health = { status?: string; db?: boolean };
    type Me = {
        user: { id: string; display_name: string; created_at: string };
        characters: Array<{ id: string; character_id: number; character_name: string }>;
    };

    let health = $state<Health | null>(null);
    let healthError = $state<string | null>(null);
    let me = $state<Me | null>(null);

    onMount(async () => {
        const [healthRes, meRes] = await Promise.allSettled([
            fetch('/api/healthz'),
            fetch('/api/me', { credentials: 'include' })
        ]);

        if (healthRes.status === 'fulfilled' && healthRes.value.ok) {
            health = await healthRes.value.json();
        } else if (healthRes.status === 'fulfilled') {
            healthError = `HTTP ${healthRes.value.status}`;
        } else {
            healthError = String(healthRes.reason);
        }

        if (meRes.status === 'fulfilled' && meRes.value.ok) {
            me = await meRes.value.json();
        }
    });

    async function logout() {
        await fetch('/api/auth/logout', { method: 'POST', credentials: 'include' });
        me = null;
    }
</script>

<h1>JitaCart</h1>
<p>Wormhole logistics, contract-settled.</p>

<section>
    <h2>Account</h2>
    {#if me}
        <p>Signed in as <strong>{me.user.display_name}</strong>.</p>
        <p><a href="/me">Profile &amp; characters →</a></p>
        <button onclick={logout}>Log out</button>
    {:else}
        <p>Sign in with EVE to link a character.</p>
        <a class="btn" href="/api/auth/eve/login">Log in with EVE Online</a>
    {/if}
</section>

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
    .btn {
        display: inline-block;
        background: #2f81f7;
        color: white;
        padding: 0.5rem 0.9rem;
        border-radius: 6px;
        text-decoration: none;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.4rem 0.8rem;
        border-radius: 6px;
        cursor: pointer;
    }
</style>
