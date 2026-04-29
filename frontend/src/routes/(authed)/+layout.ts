import { loadMe } from '$lib/stores/me';

export const ssr = false;

export async function load() {
	await loadMe();
	return {};
}
