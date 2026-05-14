<script lang="ts">
    let { data } = $props();

    // Server-validated payload from the (authed) layout load — rendered in
    // the initial SSR HTML, no client round-trip.
    const me = $derived(data.me);
    const characters = $derived(me.characters);
</script>

<p><a href="/">← Home</a></p>
<h1>Profile</h1>

<p>
    Signed in as <strong>{me.user.display_name}</strong>
    <span style="color: #8b949e">({me.user.id})</span>
</p>

<h2>Linked characters</h2>
{#if characters.length === 0}
    <p>No characters linked yet.</p>
{:else}
    <ul>
        {#each characters as c (c.id)}
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
