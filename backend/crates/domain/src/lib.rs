//! Shared domain types.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use uuid::Uuid;

pub mod multibuy;
pub mod principals;

// ── ESI id newtypes ───────────────────────────────────────────────────────────
//
// EVE Online ESI gives us a small zoo of bare integer ids — character_id,
// type_id, region_id, esi_location_id, esi_contract_id, esi_corporation_id, …
// — that are freely interchangeable at every call site if you let them stay
// `i64`. Wrapping them as transparent newtypes makes argument transposition
// a compile error while preserving:
//
//   - `#[serde(transparent)]`  → JSON wire format is byte-identical.
//   - `#[sqlx(transparent)]`   → sqlx encodes/decodes as the inner integer,
//                                so DB column types don't have to change.
//
// Callers go through `.get()` / `From<i64>` to cross the boundary.
macro_rules! esi_id_newtype {
    ($(#[$meta:meta])* $name:ident, $inner:ty) => {
        $(#[$meta])*
        #[derive(
            Copy,
            Clone,
            Eq,
            PartialEq,
            Hash,
            Debug,
            Serialize,
            Deserialize,
            sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub $inner);

        impl $name {
            #[inline]
            pub fn get(self) -> $inner {
                self.0
            }
        }

        impl From<$inner> for $name {
            #[inline]
            fn from(v: $inner) -> Self {
                Self(v)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

esi_id_newtype!(EsiCharacterId, i64);
esi_id_newtype!(EsiCorporationId, i64);
esi_id_newtype!(EsiContractId, i64);
esi_id_newtype!(EsiLocationId, i64);
esi_id_newtype!(EsiRegionId, i64);
esi_id_newtype!(EsiStructureId, i64);
esi_id_newtype!(EsiJournalRefId, i64);
esi_id_newtype!(EsiRecordId, i64);
// type_id is i32 because that's what ESI gives us (and what
// `contract_items.type_id` already is). DB columns that hold it as bigint
// will need an explicit `.get() as i64` / `i64::from(.get())` at the
// boundary; do that cast at the SQL bind site, not in the wrapper.
esi_id_newtype!(EsiTypeId, i32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
}

/// A linked EVE character. The encrypted token columns are intentionally not
/// modeled here — they live in the `api` crate's persistence layer so the
/// domain stays free of crypto concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Character {
    pub id: Uuid,
    pub user_id: Uuid,
    pub character_id: EsiCharacterId,
    pub character_name: String,
    pub owner_hash: String,
    pub scopes: Vec<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "group_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum GroupRole {
    Owner,
    Member,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub invite_code: String,
    pub created_by_user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub default_tip_pct: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: Uuid,
    pub display_name: String,
    pub role: GroupRole,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "market_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MarketKind {
    NpcHub,
    PublicStructure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "list_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ListStatus {
    Open,
    Closed,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "list_item_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ListItemStatus {
    Open,
    Claimed,
    Bought,
    Delivered,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "claim_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ClaimStatus {
    Active,
    Released,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "fulfillment_source", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum FulfillmentSource {
    Manual,
    Contract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "reimbursement_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReimbursementStatus {
    Pending,
    Settled,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: Uuid,
    pub kind: MarketKind,
    pub esi_location_id: EsiLocationId,
    /// `None` only for citadels still pending detail-fetch resolution.
    /// NPC hubs always carry `Some(_)`; the DB CHECK constraint enforces this.
    pub region_id: Option<EsiRegionId>,
    pub name: Option<String>,
    pub short_label: Option<String>,
    pub is_hub: bool,
    pub is_public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPrice {
    pub market_id: Uuid,
    pub type_id: EsiTypeId,
    pub best_sell: Option<Decimal>,
    pub best_buy: Option<Decimal>,
    pub sell_volume: i64,
    pub buy_volume: i64,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    pub id: Uuid,
    pub group_id: Uuid,
    pub created_by_user_id: Uuid,
    pub destination_label: Option<String>,
    pub notes: Option<String>,
    pub status: ListStatus,
    pub total_estimate_isk: Decimal,
    pub tip_pct: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Corp wallet funding source. When set, reimbursements on this list are
    /// corp-funded (one per hauler, covering all items).
    pub payer_corp_id: Option<Uuid>,
    pub payer_division: Option<i16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub list_id: Uuid,
    pub type_id: EsiTypeId,
    pub type_name: String,
    pub qty_requested: i64,
    pub qty_fulfilled: i64,
    pub est_unit_price_isk: Option<Decimal>,
    pub est_priced_market_id: Option<Uuid>,
    pub status: ListItemStatus,
    pub source_line_no: Option<i32>,
    pub requested_by_user_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSummary {
    pub id: Uuid,
    pub destination_label: Option<String>,
    pub status: ListStatus,
    pub item_count: i64,
    pub total_estimate_isk: Decimal,
    pub primary_market_short_label: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveItemPrice {
    pub list_item_id: Uuid,
    pub market_id: Uuid,
    pub best_sell: Option<Decimal>,
    pub best_buy: Option<Decimal>,
    pub sell_volume: i64,
    pub buy_volume: i64,
    pub computed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub id: Uuid,
    pub list_id: Uuid,
    pub hauler_user_id: Uuid,
    pub hauler_display_name: String,
    pub status: ClaimStatus,
    pub note: Option<String>,
    pub item_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fulfillment {
    pub id: Uuid,
    pub list_item_id: Uuid,
    pub claim_id: Option<Uuid>,
    pub hauler_user_id: Uuid,
    pub hauler_character_id: Option<Uuid>,
    pub hauler_character_name: Option<String>,
    pub source: FulfillmentSource,
    pub qty: i64,
    pub unit_price_isk: Decimal,
    pub bought_at_market_id: Option<Uuid>,
    pub bought_at_market_short_label: Option<String>,
    pub bought_at_note: Option<String>,
    pub bought_at: DateTime<Utc>,
    pub reversed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reimbursement {
    pub id: Uuid,
    pub list_id: Uuid,
    /// Deprecated: NULL for corp-funded reimbursements. Use
    /// `requester_principal_id` instead.
    pub requester_user_id: Option<Uuid>,
    pub requester_display_name: String,
    pub hauler_user_id: Uuid,
    pub hauler_display_name: String,
    pub subtotal_isk: Decimal,
    pub tip_isk: Decimal,
    pub total_isk: Decimal,
    pub status: ReimbursementStatus,
    pub settled_at: Option<DateTime<Utc>>,
    pub settled_by_user_id: Option<Uuid>,
    pub contract_id: Option<Uuid>,
    /// Aggregate snapshot of the bound contract, when one is linked.
    pub contract: Option<ContractSummary>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub requester_principal_id: Uuid,
    pub hauler_principal_id: Uuid,
    pub is_corp_funded: bool,
    pub verified_by_wallet: bool,
    pub wallet_settlement_delta_isk: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "contract_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ContractType {
    ItemExchange,
    Auction,
    Courier,
    Unknown,
}

impl FromStr for ContractType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "item_exchange" => Ok(ContractType::ItemExchange),
            "auction" => Ok(ContractType::Auction),
            "courier" => Ok(ContractType::Courier),
            "unknown" => Ok(ContractType::Unknown),
            other => Err(format!("unknown contract type: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "contract_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ContractStatus {
    Outstanding,
    InProgress,
    FinishedIssuer,
    FinishedContractor,
    Finished,
    Cancelled,
    Rejected,
    Failed,
    Deleted,
    Reversed,
}

impl ContractStatus {
    pub fn is_terminal_success(self) -> bool {
        matches!(
            self,
            ContractStatus::Finished
                | ContractStatus::FinishedIssuer
                | ContractStatus::FinishedContractor
        )
    }

    pub fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            ContractStatus::Cancelled
                | ContractStatus::Rejected
                | ContractStatus::Failed
                | ContractStatus::Deleted
                | ContractStatus::Reversed
        )
    }
}

impl FromStr for ContractStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "outstanding" => Ok(ContractStatus::Outstanding),
            "in_progress" => Ok(ContractStatus::InProgress),
            "finished_issuer" => Ok(ContractStatus::FinishedIssuer),
            "finished_contractor" => Ok(ContractStatus::FinishedContractor),
            "finished" => Ok(ContractStatus::Finished),
            "cancelled" => Ok(ContractStatus::Cancelled),
            "rejected" => Ok(ContractStatus::Rejected),
            "failed" => Ok(ContractStatus::Failed),
            "deleted" => Ok(ContractStatus::Deleted),
            "reversed" => Ok(ContractStatus::Reversed),
            other => Err(format!("unknown contract status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "contract_match_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ContractMatchState {
    Pending,
    Confirmed,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: Uuid,
    pub esi_contract_id: EsiContractId,
    pub issuer_character_id: EsiCharacterId,
    /// Deprecated: use `issuer_principal_id`.
    pub issuer_user_id: Option<Uuid>,
    pub assignee_character_id: Option<EsiCharacterId>,
    /// Deprecated: use `assignee_principal_id`.
    pub assignee_user_id: Option<Uuid>,
    pub contract_type: ContractType,
    pub status: ContractStatus,
    pub price_isk: Decimal,
    pub reward_isk: Decimal,
    pub collateral_isk: Decimal,
    pub expected_total_isk: Option<Decimal>,
    pub settlement_delta_isk: Option<Decimal>,
    pub date_issued: DateTime<Utc>,
    pub date_expired: Option<DateTime<Utc>>,
    pub date_accepted: Option<DateTime<Utc>>,
    pub date_completed: Option<DateTime<Utc>>,
    pub start_location_id: Option<EsiLocationId>,
    pub end_location_id: Option<EsiLocationId>,
    pub items_synced_at: Option<DateTime<Utc>>,
    pub issuer_principal_id: Option<Uuid>,
    pub assignee_principal_id: Option<Uuid>,
    pub wallet_verified_at: Option<DateTime<Utc>>,
    pub wallet_payout_aggregate_isk: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractItem {
    pub contract_id: Uuid,
    pub record_id: EsiRecordId,
    pub type_id: EsiTypeId,
    pub quantity: i64,
    pub is_included: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSummary {
    pub esi_contract_id: EsiContractId,
    pub status: ContractStatus,
    pub price_isk: Decimal,
    pub expected_total_isk: Option<Decimal>,
    pub settlement_delta_isk: Option<Decimal>,
    pub date_completed: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractMatchSuggestion {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub reimbursement_id: Uuid,
    pub score: Decimal,
    pub exact_match: bool,
    pub state: ContractMatchState,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by_user_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMarketRef {
    pub market_id: Uuid,
    pub short_label: Option<String>,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub list_id: Uuid,
    pub destination_label: Option<String>,
    pub status: ListStatus,
    pub created_at: DateTime<Utc>,
    pub accepted_markets: Vec<RunMarketRef>,
    pub items_open: i64,
    pub items_claimed: i64,
    pub items_bought: i64,
    pub items_delivered: i64,
    pub items_settled: i64,
    pub total_estimate_isk: Decimal,
    pub claimed_by_me: bool,
    pub my_active_claim_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDetail {
    pub list: List,
    pub items: Vec<ListItem>,
    pub markets: Vec<Market>,
    pub primary_market_id: Uuid,
    pub live_prices: Vec<LiveItemPrice>,
    pub claims: Vec<Claim>,
    pub fulfillments: Vec<Fulfillment>,
    pub reimbursements: Vec<Reimbursement>,
    pub last_hauler_character_id: Option<Uuid>,
    pub viewer_user_id: Uuid,
    pub viewer_role: GroupRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedType {
    pub type_id: EsiTypeId,
    pub type_name: String,
}

// ── Corp principals ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, strum::Display)]
#[sqlx(type_name = "principal_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum PrincipalKind {
    User,
    Corp,
}

/// Polymorphic principal — either a user or a corporation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: Uuid,
    pub kind: PrincipalKind,
    pub user_id: Option<Uuid>,
    pub corp_id: Option<Uuid>,
}

/// In-memory lookup index built from a batch DB query.
#[derive(Debug, Default)]
pub struct PrincipalIndex {
    /// EVE corporation id → corp UUID in our `corps` table.
    pub corp_by_esi_id: std::collections::HashMap<EsiCorporationId, Uuid>,
    /// EVE character id → user UUID in our `users` table.
    pub user_by_character_id: std::collections::HashMap<EsiCharacterId, Uuid>,
    /// principal row keyed by user_id.
    pub principal_by_user_id: std::collections::HashMap<Uuid, Principal>,
    /// principal row keyed by corp_id.
    pub principal_by_corp_id: std::collections::HashMap<Uuid, Principal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corp {
    pub id: Uuid,
    pub esi_corporation_id: EsiCorporationId,
    pub name: String,
    pub ticker: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_auth_error_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub contracts_next_poll_at: Option<DateTime<Utc>>,
    pub contracts_last_polled_at: Option<DateTime<Utc>>,
    pub wallet_next_poll_at: Option<DateTime<Utc>>,
    pub wallet_last_polled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupCorp {
    pub group_id: Uuid,
    pub corp_id: Uuid,
    pub linked_at: DateTime<Utc>,
    pub linked_by_user_id: Uuid,
    pub unlinked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpAmbassador {
    pub corp_id: Uuid,
    pub character_id: Uuid,
    pub character_name: String,
    pub granted_scopes: Vec<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub last_auth_error_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpWalletDivision {
    pub corp_id: Uuid,
    pub division: i16,
    pub name: Option<String>,
    pub balance_isk: Decimal,
    pub last_synced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpWalletJournalEntry {
    pub id: Uuid,
    pub corp_id: Uuid,
    pub division: i16,
    pub esi_journal_ref_id: EsiJournalRefId,
    pub date: DateTime<Utc>,
    pub ref_type: String,
    pub amount: Decimal,
    pub balance: Decimal,
    pub first_party_id: Option<i64>,
    pub second_party_id: Option<i64>,
    pub context_id: Option<i64>,
    pub context_id_type: Option<String>,
    pub reason: Option<String>,
    /// Only visible to ambassadors; stripped for other members.
    pub raw_json: Option<serde_json::Value>,
    pub first_seen_at: DateTime<Utc>,
}
