<script lang="ts">
    import { Toaster } from 'svelte-sonner';
    let { children } = $props();
</script>

<Toaster
    position="top-right"
    toastOptions={{
        style: 'background: #161b22; color: #e6edf3; border: 1px solid #30363d;'
    }}
/>

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
