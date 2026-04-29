<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';

    type Group = {
        id: string;
        name: string;
        invite_code: string;
        created_by_user_id: string;
        created_at: string;
        role: 'owner' | 'member';
        member_count: number;
    };

    let groups = $state<Group[] | null>(null);
    let error = $state<string | null>(null);
    let newName = $state('');
    let creating = $state(false);

    async function load() {
        const res = await fetch('/api/groups', { credentials: 'include' });
        if (res.status === 401) {
            goto('/');
            return;
        }
        if (!res.ok) {
            error = `HTTP ${res.status}`;
            return;
        }
        groups = await res.json();
    }

    onMount(load);

    async function createGroup(e: Event) {
        e.preventDefault();
        if (!newName.trim() || creating) return;
        creating = true;
        error = null;
        try {
            const res = await fetch('/api/groups', {
                method: 'POST',
                credentials: 'include',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ name: newName.trim() })
            });
            if (!res.ok) {
                error = await res.text();
                return;
            }
            const g: { id: string } = await res.json();
            goto(`/groups/${g.id}`);
        } finally {
            creating = false;
        }
    }
</script>

<p><a href="/">← Home</a></p>
<h1>Groups</h1>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

<section>
    <h2>New group</h2>
    <form onsubmit={createGroup}>
        <input
            type="text"
            placeholder="Group name"
            bind:value={newName}
            maxlength="80"
            required
        />
        <button type="submit" disabled={creating || !newName.trim()}>
            {creating ? 'Creating…' : 'Create'}
        </button>
    </form>
</section>

<section>
    <h2>Your groups</h2>
    {#if groups === null}
        <p>Loading…</p>
    {:else if groups.length === 0}
        <p>You're not in any groups yet.</p>
    {:else}
        <ul>
            {#each groups as g (g.id)}
                <li>
                    <a href="/groups/{g.id}"><strong>{g.name}</strong></a>
                    <span class="muted">
                        — {g.role}, {g.member_count} member{g.member_count === 1 ? '' : 's'}
                    </span>
                </li>
            {/each}
        </ul>
    {/if}
</section>

<style>
    form {
        display: flex;
        gap: 0.5rem;
    }
    input[type='text'] {
        flex: 1;
        background: #161b22;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.4rem 0.6rem;
    }
    button {
        background: #2f81f7;
        color: white;
        border: 0;
        padding: 0.4rem 0.9rem;
        border-radius: 6px;
        cursor: pointer;
    }
    button[disabled] {
        opacity: 0.6;
        cursor: not-allowed;
    }
    li {
        margin-bottom: 0.4rem;
    }
    .muted {
        color: #8b949e;
    }
</style>
