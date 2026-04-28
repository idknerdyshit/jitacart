<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';

    type Character = {
        id: string;
        character_id: number;
        character_name: string;
        owner_hash: string;
        scopes: string[];
        access_token_expires_at: string | null;
        created_at: string;
        last_refreshed_at: string | null;
    };
    type Me = {
        user: { id: string; display_name: string; created_at: string };
        characters: Character[];
    };

    let me = $state<Me | null>(null);
    let error = $state<string | null>(null);

    onMount(async () => {
        const res = await fetch('/api/me', { credentials: 'include' });
        if (res.status === 401) {
            goto('/');
            return;
        }
        if (!res.ok) {
            error = `HTTP ${res.status}`;
            return;
        }
        me = await res.json();
    });
</script>

<p><a href="/">← Home</a></p>
<h1>Profile</h1>

{#if error}
    <p style="color: #f87171">{error}</p>
{:else if me}
    <p>
        Signed in as <strong>{me.user.display_name}</strong>
        <span style="color: #8b949e">({me.user.id})</span>
    </p>

    <h2>Linked characters</h2>
    {#if me.characters.length === 0}
        <p>No characters linked yet.</p>
    {:else}
        <ul>
            {#each me.characters as c (c.id)}
                <li>
                    <strong>{c.character_name}</strong>
                    <span style="color: #8b949e">#{c.character_id}</span>
                    <div style="color: #8b949e; font-size: 0.85rem">
                        scopes: {c.scopes.join(', ') || '(none)'}
                    </div>
                </li>
            {/each}
        </ul>
    {/if}

    <p>
        <a class="btn" href="/api/auth/eve/login?attach=1">+ Add another character</a>
    </p>
{:else}
    <p>Loading…</p>
{/if}

<style>
    li {
        margin-bottom: 0.75rem;
    }
    .btn {
        display: inline-block;
        background: #2f81f7;
        color: white;
        padding: 0.4rem 0.8rem;
        border-radius: 6px;
        text-decoration: none;
    }
</style>
