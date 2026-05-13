import type { PageServerLoad } from './$types';
import type { GroupMarket } from '$lib/api';

export const load: PageServerLoad = async ({ fetch, request, params }) => {
    const cookie = request.headers.get('cookie') ?? '';
    const res = await fetch(`/api/groups/${params.id}/markets`, { headers: { cookie } });
    if (!res.ok) {
        throw new Error(`/groups/${params.id}/markets responded ${res.status}`);
    }
    const markets: GroupMarket[] = await res.json();
    return { markets };
};
