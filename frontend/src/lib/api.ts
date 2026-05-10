import { goto } from '$app/navigation';

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    const res = await fetch(`/api${path}`, { credentials: 'include', ...init });
    if (res.status === 401) {
        // 401 means the session is gone or never existed — redirect to login.
        // The thrown error gives callers a way to early-return; toasts should
        // not surface it.
        goto('/');
        throw new Error('unauthenticated');
    }
    if (!res.ok) {
        // Non-401 errors get the upstream body, but cap it so a sprawling
        // server stack trace doesn't end up in a toast.
        const body = await res.text();
        const trimmed = body.length > 200 ? `${body.slice(0, 200)}…` : body;
        throw new Error(`${res.status}: ${trimmed}`);
    }
    return res.status === 204 ? (undefined as T) : res.json();
}

export type ViewerCharacter = {
    id: string;
    character_id: number;
    character_name: string;
    owner_hash: string;
    scopes: string[];
    access_token_expires_at: string | null;
    created_at: string;
    last_refreshed_at: string | null;
};

export type Me = {
    user: { id: string; display_name: string; created_at: string };
    characters: ViewerCharacter[];
    active_character_id: string | null;
};

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

export const STALE_AFTER_MS = 2 * 600 * 1000;

export function isMarketStale(m: Pick<GroupMarket, 'is_public' | 'kind' | 'last_orders_synced_at'>): boolean {
    if (!m.is_public) return true;
    if (m.kind !== 'public_structure') return false;
    if (!m.last_orders_synced_at) return true;
    return Date.now() - new Date(m.last_orders_synced_at).getTime() > STALE_AFTER_MS;
}

export function isPriceStale(computed_at: string | null | undefined): boolean {
    if (!computed_at) return false;
    return Date.now() - new Date(computed_at).getTime() > STALE_AFTER_MS;
}

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
    tip_pct: string;
    /** Corp-funded reimbursement payer, if set. */
    payer_corp_id: string | null;
    payer_division: number | null;
    created_at: string;
    updated_at: string;
};

export type ListItemStatus = 'open' | 'claimed' | 'bought' | 'delivered' | 'settled';

export type ListItem = {
    id: string;
    list_id: string;
    type_id: number;
    type_name: string;
    qty_requested: number;
    qty_fulfilled: number;
    est_unit_price_isk: string | null;
    est_priced_market_id: string | null;
    status: ListItemStatus;
    source_line_no: number | null;
    requested_by_user_id: string;
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

export type ClaimStatus = 'active' | 'released' | 'completed';

export type Claim = {
    id: string;
    list_id: string;
    hauler_user_id: string;
    hauler_display_name: string;
    status: ClaimStatus;
    note: string | null;
    item_ids: string[];
    created_at: string;
    released_at: string | null;
};

export type FulfillmentSource = 'manual' | 'contract';

export type Fulfillment = {
    id: string;
    list_item_id: string;
    claim_id: string | null;
    hauler_user_id: string;
    hauler_character_id: string | null;
    hauler_character_name: string | null;
    source: FulfillmentSource;
    qty: number;
    unit_price_isk: string;
    bought_at_market_id: string | null;
    bought_at_market_short_label: string | null;
    bought_at_note: string | null;
    bought_at: string;
    reversed_at: string | null;
};

export type ReimbursementStatus = 'pending' | 'settled' | 'cancelled';

export type ContractStatus =
    | 'outstanding'
    | 'in_progress'
    | 'finished_issuer'
    | 'finished_contractor'
    | 'finished'
    | 'cancelled'
    | 'rejected'
    | 'failed'
    | 'deleted'
    | 'reversed';

export function isContractTerminalSuccess(s: ContractStatus | null | undefined): boolean {
    return s === 'finished' || s === 'finished_issuer' || s === 'finished_contractor';
}

export function isContractTerminalFailure(s: ContractStatus | null | undefined): boolean {
    return s === 'cancelled' || s === 'rejected' || s === 'failed' || s === 'deleted' || s === 'reversed';
}

export type ContractSummary = {
    esi_contract_id: number;
    status: ContractStatus;
    price_isk: string;
    expected_total_isk: string | null;
    settlement_delta_isk: string | null;
    date_completed: string | null;
};

export type Reimbursement = {
    id: string;
    list_id: string;
    /** Null when this is a corp-funded reimbursement (hauler paid by corp). */
    requester_user_id: string | null;
    requester_display_name: string;
    hauler_user_id: string;
    hauler_display_name: string;
    subtotal_isk: string;
    tip_isk: string;
    total_isk: string;
    status: ReimbursementStatus;
    settled_at: string | null;
    settled_by_user_id: string | null;
    contract_id: string | null;
    contract: ContractSummary | null;
    is_corp_funded: boolean;
    verified_by_wallet: boolean;
    wallet_settlement_delta_isk: string | null;
    created_at: string;
    updated_at: string;
};

export type ContractSuggestion = {
    id: string;
    contract_id: string;
    esi_contract_id: number;
    contract_status: ContractStatus;
    contract_price_isk: string;
    contract_expected_total_isk: string | null;
    reimbursement_id: string;
    list_id: string;
    list_destination_label: string | null;
    requester_display_name: string;
    hauler_display_name: string;
    reimbursement_total_isk: string;
    score: string;
    exact_match: boolean;
    state: 'pending' | 'confirmed' | 'rejected' | 'superseded';
    created_at: string;
    decided_at: string | null;
};

export type BoundContract = {
    contract_id: string;
    esi_contract_id: number;
    status: ContractStatus;
    price_isk: string;
    expected_total_isk: string | null;
    settlement_delta_isk: string | null;
    date_completed: string | null;
    bound_reimbursement_count: number;
};

export type ListDetail = {
    list: List;
    items: ListItem[];
    markets: Market[];
    primary_market_id: string;
    live_prices: LiveItemPrice[];
    claims: Claim[];
    fulfillments: Fulfillment[];
    reimbursements: Reimbursement[];
    last_hauler_character_id: string | null;
    viewer_user_id: string;
    viewer_role: 'owner' | 'member';
};

export type RunMarketRef = {
    market_id: string;
    short_label: string | null;
    is_primary: boolean;
};

export type RunSummary = {
    list_id: string;
    destination_label: string | null;
    status: ListStatus;
    created_at: string;
    accepted_markets: RunMarketRef[];
    items_open: number;
    items_claimed: number;
    items_bought: number;
    items_delivered: number;
    items_settled: number;
    total_estimate_isk: string;
    claimed_by_me: boolean;
    my_active_claim_id: string | null;
};

export type GroupRole = 'owner' | 'member';

export type Group = {
    id: string;
    name: string;
    invite_code: string;
    created_by_user_id: string;
    created_at: string;
    default_tip_pct: string;
};

export type GroupSummary = Group & {
    role: GroupRole;
    member_count: number;
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

// ── Corps ─────────────────────────────────────────────────────────────────────

export type Corp = {
    id: string;
    esi_corporation_id: number;
    name: string;
    ticker: string;
    linked_by_user_id: string;
    linked_at: string;
    disabled_at: string | null;
};

export type CorpAmbassador = {
    character_id: string;
    character_name: string;
    granted_scopes: string[];
    last_used_at: string | null;
    last_auth_error_at: string | null;
    disabled_at: string | null;
};

export type CorpWalletDivision = {
    division: number;
    name?: string;
    /** Null when the caller is not an ambassador or group owner. */
    balance_isk: string | null;
    last_synced_at: string | null;
};

export type CorpJournalEntry = {
    id: string;
    division: number;
    esi_journal_ref_id: number;
    date: string;
    ref_type: string;
    amount: string;
    balance: string;
    first_party_id: number | null;
    second_party_id: number | null;
    context_id: number | null;
    context_id_type: string | null;
    reason: string | null;
    /** Only present if viewer is an ambassador or group owner. */
    raw_json: unknown | null;
};

export type CorpDetail = {
    corp: Corp;
    ambassadors: CorpAmbassador[];
    wallet_divisions: CorpWalletDivision[];
    role: 'owner' | 'member';
    is_ambassador: boolean;
};

export function fmtIsk(v: string | null | undefined): string {
    if (v == null) return '—';
    const n = Number(v);
    if (!isFinite(n)) return v;
    return n.toLocaleString('en-US', { maximumFractionDigits: 2 }) + ' ISK';
}

export function fmtPct(v: string | null | undefined): string {
    if (v == null) return '—';
    const n = Number(v) * 100;
    if (!isFinite(n)) return v;
    return n.toLocaleString('en-US', { maximumFractionDigits: 2 }) + '%';
}

export function deltaClass(d: string | null | undefined): '' | 'pos' | 'neg' {
    if (!d) return '';
    const n = Number(d);
    if (n > 0) return 'pos';
    if (n < 0) return 'neg';
    return '';
}

export function findViewerClaim(detail: ListDetail): Claim | null {
    return (
        detail.claims.find(
            (c) => c.hauler_user_id === detail.viewer_user_id && c.status === 'active'
        ) ?? null
    );
}

export type WebhookConfig = {
    webhook_url: string;
    notify_list_created: boolean;
    notify_list_claimed: boolean;
    notify_list_delivered: boolean;
    notify_reimbursement_settled: boolean;
};
