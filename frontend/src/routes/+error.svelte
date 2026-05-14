<script lang="ts">
    import { page } from '$app/state';

    const status = $derived(page.status);
    const message = $derived(page.error?.message ?? 'Something went wrong.');
</script>

<h1>{status === 403 ? 'Access denied' : `Error ${status}`}</h1>
<p class="message">{message}</p>

<p>
    <a class="btn" href="/">← Back to home</a>
</p>

<style>
    .message {
        color: #f87171;
    }
    .btn {
        display: inline-block;
        background: #2f81f7;
        color: white;
        padding: 0.5rem 0.9rem;
        border-radius: 6px;
        text-decoration: none;
    }
</style>
