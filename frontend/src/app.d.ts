// See https://kit.svelte.dev/docs/types#app
import type { Me } from '$lib/api';

declare global {
    namespace App {
        // interface Error {}
        interface Locals {
            /** `/api/me` payload for this request, or null if unauthenticated. */
            me: Me | null;
            /** HTTP status from `/api/me` (0 when no session cookie was sent). */
            meStatus: number;
        }
        // interface PageData {}
        // interface PageState {}
        // interface Platform {}
    }
}

export {};
