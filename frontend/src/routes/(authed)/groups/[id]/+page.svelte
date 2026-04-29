<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { page } from '$app/state';
    import { api, fmtPct, type Group } from '$lib/api';

    type Member = {
        user_id: string;
        display_name: string;
        role: 'owner' | 'member';
        joined_at: string;
    };
    type Detail = {
        group: Group;
        role: 'owner' | 'member';
        members: Member[];
    };

    let detail = $state<Detail | null>(null);
    let error = $state<string | null>(null);
    let copyMsg = $state<string | null>(null);

    let editingTip = $state(false);
    let tipInput = $state('');
    let savingTip = $state(false);

    const groupId = $derived(page.params.id);

    async function load() {
        error = null;
        const res = await fetch(`/api/groups/${groupId}`, { credentials: 'include' });
        if (res.status === 401) {
            goto('/');
            return;
        }
        if (!res.ok) {
            error = `HTTP ${res.status}`;
            return;
        }
        detail = await res.json();
    }

    onMount(load);

    function inviteUrl(code: string) {
        return `${location.origin}/g/join/${code}`;
    }

    async function copyInvite() {
        if (!detail) return;
        await navigator.clipboard.writeText(inviteUrl(detail.group.invite_code));
        copyMsg = 'Copied!';
        setTimeout(() => (copyMsg = null), 1500);
    }

    async function rotateInvite() {
        if (!detail) return;
        if (!confirm('Rotate the invite code? Existing links will stop working.')) return;
        const res = await fetch(`/api/groups/${groupId}/rotate-invite`, {
            method: 'POST',
            credentials: 'include'
        });
        if (!res.ok) {
            error = await res.text();
            return;
        }
        const updated: Group = await res.json();
        detail = { ...detail, group: updated };
    }

    async function leave() {
        if (!detail) return;
        if (!confirm(`Leave "${detail.group.name}"?`)) return;
        const res = await fetch(`/api/groups/${groupId}/leave`, {
            method: 'POST',
            credentials: 'include'
        });
        if (!res.ok) {
            error = await res.text();
            return;
        }
        goto('/groups');
    }

    async function deleteGroup() {
        if (!detail) return;
        if (
            !confirm(
                `Delete "${detail.group.name}"? This removes the group for every member.`
            )
        ) {
            return;
        }
        const res = await fetch(`/api/groups/${groupId}`, {
            method: 'DELETE',
            credentials: 'include'
        });
        if (!res.ok) {
            error = await res.text();
            return;
        }
        goto('/groups');
    }

    function startEditTip() {
        if (!detail) return;
        tipInput = (Number(detail.group.default_tip_pct) * 100).toFixed(2);
        editingTip = true;
    }

    async function saveTip() {
        if (!detail || savingTip) return;
        const pct = Number(tipInput);
        if (isNaN(pct) || pct < 0 || pct > 100) {
            error = 'Tip must be 0–100%';
            return;
        }
        savingTip = true;
        try {
            const updated = await api<Group>(`/groups/${groupId}/default-tip`, {
                method: 'PATCH',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify({ tip_pct: pct / 100 })
            });
            detail = { ...detail, group: updated };
            editingTip = false;
        } catch (e) {
            error = (e as Error).message;
        } finally {
            savingTip = false;
        }
    }
</script>

<p><a href="/groups">← Groups</a></p>

{#if error}
    <p style="color: #f87171">{error}</p>
{/if}

{#if detail}
    <h1>{detail.group.name}</h1>
    <p class="muted">Your role: {detail.role}</p>

    <section>
        <h2>Invite link</h2>
        <code>{inviteUrl(detail.group.invite_code)}</code>
        <div class="actions">
            <button onclick={copyInvite}>Copy</button>
            {#if detail.role === 'owner'}
                <button onclick={rotateInvite}>Rotate</button>
            {/if}
            {#if copyMsg}<span class="muted">{copyMsg}</span>{/if}
        </div>
    </section>

    <section>
        <h2>Shopping lists</h2>
        <div class="nav-row">
            <a href={`/groups/${groupId}/lists`}>Lists →</a>
            <a href={`/groups/${groupId}/runs`}>Runs →</a>
        </div>
    </section>

    <section>
        <h2>Markets</h2>
        <p><a href={`/groups/${groupId}/tracked-markets`}>Tracked citadels →</a></p>
    </section>

    {#if detail.role === 'owner'}
        <section>
            <h2>Default tip</h2>
            {#if editingTip}
                <div class="tip-row">
                    <input
                        type="number"
                        min="0"
                        max="100"
                        step="0.1"
                        bind:value={tipInput}
                        style="width: 6rem"
                    />
                    <span class="muted">%</span>
                    <button class="primary" disabled={savingTip} onclick={saveTip}>Save</button>
                    <button onclick={() => (editingTip = false)}>Cancel</button>
                </div>
            {:else}
                <div class="tip-row">
                    <span>{fmtPct(detail.group.default_tip_pct)}</span>
                    <button onclick={startEditTip}>Edit</button>
                </div>
            {/if}
            <p class="muted small">New lists inherit this tip percentage.</p>
        </section>
    {/if}

    <section>
        <h2>Members</h2>
        <ul>
            {#each detail.members as m (m.user_id)}
                <li>
                    <strong>{m.display_name}</strong>
                    <span class="muted">— {m.role}</span>
                </li>
            {/each}
        </ul>
    </section>

    <section>
        {#if detail.role === 'owner'}
            <button class="danger" onclick={deleteGroup}>Delete group</button>
        {:else}
            <button class="danger" onclick={leave}>Leave group</button>
        {/if}
    </section>
{:else if !error}
    <p>Loading…</p>
{/if}

<style>
    code {
        background: #161b22;
        padding: 0.4rem 0.6rem;
        border-radius: 6px;
        display: inline-block;
        word-break: break-all;
    }
    .actions {
        margin-top: 0.5rem;
        display: flex;
        gap: 0.5rem;
        align-items: center;
    }
    .nav-row {
        display: flex;
        gap: 1.5rem;
        margin-top: 0.25rem;
    }
    .tip-row {
        display: flex;
        gap: 0.5rem;
        align-items: center;
    }
    button {
        background: #21262d;
        color: #e6edf3;
        border: 1px solid #30363d;
        padding: 0.35rem 0.75rem;
        border-radius: 6px;
        cursor: pointer;
    }
    button.primary {
        border-color: #2f6feb;
    }
    button.danger {
        border-color: #6e2832;
        color: #f87171;
    }
    button:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    li {
        margin-bottom: 0.3rem;
    }
    .muted {
        color: #8b949e;
    }
    .small {
        font-size: 0.85rem;
        margin-top: 0.25rem;
    }
    input[type='number'] {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
    }
    section {
        margin-top: 1.25rem;
    }
</style>
