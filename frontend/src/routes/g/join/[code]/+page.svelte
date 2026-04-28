<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { page } from '$app/state';

    let status = $state<'working' | 'error' | 'unauth'>('working');
    let message = $state<string | null>(null);

    const code = $derived(page.params.code ?? '');
    const loginHref = $derived(
        `/api/auth/eve/login?return_to=${encodeURIComponent(`/g/join/${code}`)}`
    );

    onMount(async () => {
        const res = await fetch(`/api/groups/join/${encodeURIComponent(code)}`, {
            method: 'POST',
            credentials: 'include'
        });
        if (res.status === 401) {
            status = 'unauth';
            return;
        }
        if (!res.ok) {
            status = 'error';
            message = (await res.text()) || `HTTP ${res.status}`;
            return;
        }
        const g: { id: string } = await res.json();
        goto(`/groups/${g.id}`);
    });
</script>

<h1>Join group</h1>
{#if status === 'working'}
    <p>Joining…</p>
{:else if status === 'unauth'}
    <p>You need to log in first to accept this invite.</p>
    <p>
        <a class="btn" href={loginHref}>Log in with EVE Online</a>
    </p>
    <p class="muted">After logging in, you'll return to this invite.</p>
{:else}
    <p style="color: #f87171">Couldn't join: {message}</p>
    <p><a href="/groups">Back to groups</a></p>
{/if}

<style>
    .btn {
        display: inline-block;
        background: #2f81f7;
        color: white;
        padding: 0.5rem 0.9rem;
        border-radius: 6px;
        text-decoration: none;
    }
    .muted {
        color: #8b949e;
    }
</style>
