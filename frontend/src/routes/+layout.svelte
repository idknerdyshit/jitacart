<script lang="ts">
    let { children } = $props();
</script>

<main>
    {@render children()}
</main>

<style>
    :global(body) {
        margin: 0;
        font-family: system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif;
        background: #0e1116;
        color: #e6edf3;
    }
    main {
        max-width: 720px;
        margin: 4rem auto;
        padding: 0 1.5rem;
    }
</style>
