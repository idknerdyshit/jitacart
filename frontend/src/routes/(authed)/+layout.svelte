<script lang="ts">
    import { me } from '$lib/stores/me';
    import { activeCharacter, setActiveCharacter } from '$lib/stores/activeCharacter';

    let { children } = $props();

    let pickerOpen = $state(false);
    let switching = $state(false);

    const chars = $derived($me?.characters ?? []);
    const active = $derived($activeCharacter);
    const portraitUrl = $derived(
        active
            ? `https://images.evetech.net/characters/${active.character_id}/portrait?size=32`
            : null
    );

    async function switchTo(id: string | null) {
        if (switching) return;
        switching = true;
        try {
            await setActiveCharacter(id);
        } finally {
            switching = false;
            pickerOpen = false;
        }
    }
</script>

<nav class="topbar">
    <a class="brand" href="/groups">JitaCart</a>
    <div class="spacer"></div>
    {#if $me}
        <div class="char-picker">
            <button class="char-btn" onclick={() => (pickerOpen = !pickerOpen)} type="button">
                {#if portraitUrl}
                    <img src={portraitUrl} alt="" class="portrait" />
                {/if}
                <span class="char-name">{active?.character_name ?? $me.user.display_name}</span>
                <span class="caret">▾</span>
            </button>
            {#if pickerOpen}
                <div class="dropdown">
                    {#each chars as c (c.id)}
                        <button
                            class="dropdown-item"
                            class:active={c.id === active?.id}
                            disabled={switching}
                            onclick={() => switchTo(c.id)}
                            type="button"
                        >
                            <img
                                src="https://images.evetech.net/characters/{c.character_id}/portrait?size=32"
                                alt=""
                                class="portrait-sm"
                            />
                            {c.character_name}
                        </button>
                    {/each}
                    {#if active}
                        <button
                            class="dropdown-item muted"
                            disabled={switching}
                            onclick={() => switchTo(null)}
                            type="button"
                        >
                            Clear selection
                        </button>
                    {/if}
                </div>
            {/if}
        </div>
    {/if}
</nav>

{@render children()}

<style>
    .topbar {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 0.5rem 1rem;
        background: #161b22;
        border-bottom: 1px solid #21262d;
        margin-bottom: 1rem;
    }
    .brand {
        font-weight: 700;
        color: #e6edf3;
        text-decoration: none;
        font-size: 1rem;
    }
    .spacer {
        flex: 1;
    }
    .char-picker {
        position: relative;
    }
    .char-btn {
        display: flex;
        align-items: center;
        gap: 0.4rem;
        background: #21262d;
        border: 1px solid #30363d;
        color: #e6edf3;
        padding: 0.25rem 0.6rem;
        border-radius: 6px;
        cursor: pointer;
        font-size: 0.85rem;
    }
    .portrait {
        width: 24px;
        height: 24px;
        border-radius: 4px;
    }
    .portrait-sm {
        width: 20px;
        height: 20px;
        border-radius: 3px;
    }
    .char-name {
        max-width: 140px;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
    }
    .caret {
        font-size: 0.7rem;
        color: #8b949e;
    }
    .dropdown {
        position: absolute;
        top: 100%;
        right: 0;
        margin-top: 0.3rem;
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 8px;
        min-width: 200px;
        z-index: 50;
        display: flex;
        flex-direction: column;
        padding: 0.3rem;
    }
    .dropdown-item {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        padding: 0.4rem 0.6rem;
        background: none;
        border: none;
        color: #e6edf3;
        cursor: pointer;
        border-radius: 4px;
        font-size: 0.85rem;
        text-align: left;
        width: 100%;
    }
    .dropdown-item:hover {
        background: #21262d;
    }
    .dropdown-item.active {
        color: #79c0ff;
    }
    .dropdown-item.muted {
        color: #8b949e;
        font-size: 0.8rem;
    }
    .dropdown-item:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }
    @media (max-width: 640px) {
        .topbar {
            flex-wrap: wrap;
            gap: 0.5rem;
            padding: 0.5rem 0.75rem;
        }
        .char-name {
            display: none;
        }
        .char-btn {
            padding: 0.25rem 0.45rem;
        }
    }
</style>
