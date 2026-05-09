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

    @media (max-width: 768px) {
        :global(.responsive-table thead) {
            display: none;
        }
        :global(.responsive-table tbody tr) {
            display: block;
            background: #161b22;
            border: 1px solid #30363d;
            border-radius: 8px;
            padding: 0.75rem;
            margin-bottom: 0.75rem;
        }
        :global(.responsive-table td) {
            display: flex;
            justify-content: space-between;
            border-bottom: none;
            padding: 0.2rem 0;
        }
        :global(.responsive-table td::before) {
            content: attr(data-label);
            font-weight: 600;
            color: #8b949e;
            margin-right: 0.5rem;
        }
        :global(.responsive-table td.actions-cell) {
            justify-content: flex-end;
        }
        :global(.responsive-table td.actions-cell::before) {
            display: none;
        }
    }
</style>
