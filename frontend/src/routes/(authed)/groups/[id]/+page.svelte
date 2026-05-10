<script lang="ts">
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { page } from '$app/state';
    import { api, fmtPct, type Group, type WebhookConfig } from '$lib/api';
    import { toast } from 'svelte-sonner';

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

    let editingTip = $state(false);
    let tipInput = $state('');
    let savingTip = $state(false);

    const emptyDraft = (): WebhookConfig => ({
        webhook_url: '',
        notify_list_created: true,
        notify_list_claimed: true,
        notify_list_delivered: true,
        notify_reimbursement_settled: true
    });

    let webhookLoaded = $state(false);
    let savingWebhook = $state(false);
    let draft = $state<WebhookConfig>(emptyDraft());
    let savedSnapshot = $state<WebhookConfig | null>(null);
    const dirty = $derived(
        savedSnapshot === null
            ? draft.webhook_url.trim().length > 0
            : draft.webhook_url !== savedSnapshot.webhook_url ||
              draft.notify_list_created !== savedSnapshot.notify_list_created ||
              draft.notify_list_claimed !== savedSnapshot.notify_list_claimed ||
              draft.notify_list_delivered !== savedSnapshot.notify_list_delivered ||
              draft.notify_reimbursement_settled !== savedSnapshot.notify_reimbursement_settled
    );

    const groupId = $derived(page.params.id);

    async function load() {
        error = null;
        try {
            detail = await api<Detail>(`/groups/${groupId}`);
        } catch (e) {
            error = (e as Error).message;
        }
    }

    onMount(async () => {
        await load();
        if (detail?.role === 'owner') {
            try {
                const cfg = await api<WebhookConfig | null>(`/groups/${groupId}/webhook`);
                if (cfg) {
                    savedSnapshot = cfg;
                    draft = { ...cfg };
                }
            } catch {
                // non-fatal
            }
            webhookLoaded = true;
        }
    });

    function inviteUrl(code: string) {
        return `${location.origin}/g/join/${code}`;
    }

    async function copyInvite() {
        if (!detail) return;
        await navigator.clipboard.writeText(inviteUrl(detail.group.invite_code));
        toast.success('Invite link copied');
    }

    async function rotateInvite() {
        if (!detail) return;
        if (!confirm('Rotate the invite code? Existing links will stop working.')) return;
        try {
            const updated = await api<Group>(`/groups/${groupId}/rotate-invite`, {
                method: 'POST'
            });
            detail = { ...detail, group: updated };
            toast.success('Invite code rotated');
        } catch (e) {
            error = (e as Error).message;
        }
    }

    async function leave() {
        if (!detail) return;
        if (!confirm(`Leave "${detail.group.name}"?`)) return;
        try {
            await api(`/groups/${groupId}/leave`, { method: 'POST' });
            goto('/groups');
        } catch (e) {
            error = (e as Error).message;
        }
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
        try {
            await api(`/groups/${groupId}`, { method: 'DELETE' });
            goto('/groups');
        } catch (e) {
            error = (e as Error).message;
        }
    }

    function startEditTip() {
        if (!detail) return;
        tipInput = (Number(detail.group.default_tip_pct) * 100).toFixed(2);
        editingTip = true;
    }

    async function saveWebhook() {
        if (savingWebhook) return;
        savingWebhook = true;
        try {
            const saved = await api<WebhookConfig>(`/groups/${groupId}/webhook`, {
                method: 'PUT',
                headers: { 'content-type': 'application/json' },
                body: JSON.stringify(draft)
            });
            savedSnapshot = saved;
            draft = { ...saved };
            toast.success('Webhook saved');
        } catch (e) {
            error = (e as Error).message;
            toast.error(error ?? 'Failed to save webhook');
        } finally {
            savingWebhook = false;
        }
    }

    async function removeWebhook() {
        if (!confirm('Remove the Discord webhook?')) return;
        try {
            await api(`/groups/${groupId}/webhook`, { method: 'DELETE' });
            savedSnapshot = null;
            draft = emptyDraft();
            toast.success('Webhook removed');
        } catch (e) {
            error = (e as Error).message;
            toast.error(error ?? 'Failed to remove webhook');
        }
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
            toast.success('Default tip updated');
        } catch (e) {
            error = (e as Error).message;
            toast.error(error ?? 'Failed to update tip');
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

    <section>
        <h2>Corp wallets</h2>
        <p><a href={`/groups/${groupId}/corps`}>Linked corps &amp; ambassadors →</a></p>
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

    {#if detail.role === 'owner' && webhookLoaded}
        <section>
            <h2>Discord Notifications</h2>
            <label class="wh-label">
                Webhook URL
                <input
                    type="text"
                    placeholder="https://discord.com/api/webhooks/..."
                    bind:value={draft.webhook_url}
                    class="wh-url"
                />
            </label>
            <div class="wh-toggles">
                <label><input type="checkbox" bind:checked={draft.notify_list_created} /> List created</label>
                <label><input type="checkbox" bind:checked={draft.notify_list_claimed} /> List claimed</label>
                <label><input type="checkbox" bind:checked={draft.notify_list_delivered} /> List delivered</label>
                <label><input type="checkbox" bind:checked={draft.notify_reimbursement_settled} /> Reimbursement settled</label>
            </div>
            <div class="actions">
                <button
                    class="primary"
                    disabled={savingWebhook || !draft.webhook_url.trim() || !dirty}
                    onclick={saveWebhook}
                >
                    {savingWebhook ? 'Saving…' : 'Save'}
                </button>
                {#if savedSnapshot}
                    <button class="danger" onclick={removeWebhook}>Remove</button>
                {/if}
            </div>
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
    .wh-label {
        display: flex;
        flex-direction: column;
        gap: 0.3rem;
        font-size: 0.9rem;
        color: #8b949e;
        margin-bottom: 0.5rem;
    }
    .wh-url {
        background: #0d1117;
        color: #e6edf3;
        border: 1px solid #30363d;
        border-radius: 6px;
        padding: 0.3rem 0.5rem;
        width: 100%;
    }
    .wh-toggles {
        display: flex;
        flex-direction: column;
        gap: 0.3rem;
        margin-bottom: 0.5rem;
        font-size: 0.9rem;
    }
    .wh-toggles label {
        display: flex;
        align-items: center;
        gap: 0.4rem;
    }
    @media (max-width: 640px) {
        code {
            font-size: 0.8rem;
        }
        .actions {
            flex-wrap: wrap;
        }
        .nav-row {
            flex-direction: column;
            gap: 0.25rem;
        }
        .tip-row {
            flex-wrap: wrap;
        }
    }
</style>
