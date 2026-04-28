import { goto } from '$app/navigation';

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    const res = await fetch(`/api${path}`, { credentials: 'include', ...init });
    if (res.status === 401) {
        goto('/');
        throw new Error('unauthenticated');
    }
    if (!res.ok) {
        throw new Error(`${res.status}: ${await res.text()}`);
    }
    return res.status === 204 ? (undefined as T) : res.json();
}

export type Market = {
    id: string;
    kind: 'npc_hub' | 'public_structure';
    esi_location_id: number;
    /** NPC hubs always carry these; citadels may be null until detail-fetch resolves them. */
    region_id: number | null;
    name: string | null;
    short_label: string | null;
    is_hub: boolean;
    is_public: boolean;
};

export type GroupMarket = Market & {
    last_orders_synced_at: string | null;
    untrackable_until: string | null;
    accessing_character_id: string | null;
    accessing_character_name: string | null;
};

export type ListStatus = 'open' | 'closed' | 'archived';

export type ListSummary = {
    id: string;
    destination_label: string | null;
    status: ListStatus;
    item_count: number;
    total_estimate_isk: string;
    primary_market_short_label: string | null;
    created_at: string;
};

export type List = {
    id: string;
    group_id: string;
    created_by_user_id: string;
    destination_label: string | null;
    notes: string | null;
    status: ListStatus;
    total_estimate_isk: string;
    created_at: string;
    updated_at: string;
};

export type ListItem = {
    id: string;
    list_id: string;
    type_id: number;
    type_name: string;
    qty_requested: number;
    qty_fulfilled: number;
    est_unit_price_isk: string | null;
    est_priced_market_id: string | null;
    status: 'open' | 'claimed' | 'bought' | 'delivered' | 'settled';
    source_line_no: number | null;
};

export type LiveItemPrice = {
    list_item_id: string;
    market_id: string;
    best_sell: string | null;
    best_buy: string | null;
    sell_volume: number;
    buy_volume: number;
    computed_at: string | null;
};

export type ListDetail = {
    list: List;
    items: ListItem[];
    markets: Market[];
    primary_market_id: string;
    live_prices: LiveItemPrice[];
};

export type PreviewPrice = {
    best_sell: string | null;
    best_buy: string | null;
    sell_volume: number;
    buy_volume: number;
    computed_at: string | null;
};

export type PreviewLine = {
    line_nos: number[];
    name: string;
    type_id: number | null;
    type_name: string | null;
    qty: number;
    prices: Record<string, PreviewPrice>;
    error: string | null;
};

export type PreviewResponse = {
    lines: PreviewLine[];
    unresolved_names: string[];
    errors: { line_no: number; raw: string; reason: string }[];
};

export function fmtIsk(v: string | null | undefined): string {
    if (v == null) return '—';
    const n = Number(v);
    if (!isFinite(n)) return v;
    return n.toLocaleString('en-US', { maximumFractionDigits: 2 }) + ' ISK';
}
