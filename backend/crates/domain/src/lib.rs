//! Shared domain types.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

pub mod multibuy;

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
    pub character_id: i64,
    pub character_name: String,
    pub owner_hash: String,
    pub scopes: Vec<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupRole {
    Owner,
    Member,
}

impl GroupRole {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupRole::Owner => "owner",
            GroupRole::Member => "member",
        }
    }
}

impl fmt::Display for GroupRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GroupRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(GroupRole::Owner),
            "member" => Ok(GroupRole::Member),
            other => Err(format!("unknown group role: {other}")),
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketKind {
    NpcHub,
    PublicStructure,
}

impl MarketKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MarketKind::NpcHub => "npc_hub",
            MarketKind::PublicStructure => "public_structure",
        }
    }
}

impl fmt::Display for MarketKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MarketKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "npc_hub" => Ok(MarketKind::NpcHub),
            "public_structure" => Ok(MarketKind::PublicStructure),
            other => Err(format!("unknown market kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListStatus {
    Open,
    Closed,
    Archived,
}

impl ListStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ListStatus::Open => "open",
            ListStatus::Closed => "closed",
            ListStatus::Archived => "archived",
        }
    }
}

impl fmt::Display for ListStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ListStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(ListStatus::Open),
            "closed" => Ok(ListStatus::Closed),
            "archived" => Ok(ListStatus::Archived),
            other => Err(format!("unknown list status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListItemStatus {
    Open,
    Claimed,
    Bought,
    Delivered,
    Settled,
}

impl ListItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ListItemStatus::Open => "open",
            ListItemStatus::Claimed => "claimed",
            ListItemStatus::Bought => "bought",
            ListItemStatus::Delivered => "delivered",
            ListItemStatus::Settled => "settled",
        }
    }
}

impl fmt::Display for ListItemStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ListItemStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(ListItemStatus::Open),
            "claimed" => Ok(ListItemStatus::Claimed),
            "bought" => Ok(ListItemStatus::Bought),
            "delivered" => Ok(ListItemStatus::Delivered),
            "settled" => Ok(ListItemStatus::Settled),
            other => Err(format!("unknown list item status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimStatus {
    Active,
    Released,
    Completed,
}

impl ClaimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimStatus::Active => "active",
            ClaimStatus::Released => "released",
            ClaimStatus::Completed => "completed",
        }
    }
}

impl fmt::Display for ClaimStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ClaimStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(ClaimStatus::Active),
            "released" => Ok(ClaimStatus::Released),
            "completed" => Ok(ClaimStatus::Completed),
            other => Err(format!("unknown claim status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FulfillmentSource {
    Manual,
    Contract,
}

impl FulfillmentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            FulfillmentSource::Manual => "manual",
            FulfillmentSource::Contract => "contract",
        }
    }
}

impl fmt::Display for FulfillmentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FulfillmentSource {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "manual" => Ok(FulfillmentSource::Manual),
            "contract" => Ok(FulfillmentSource::Contract),
            other => Err(format!("unknown fulfillment source: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReimbursementStatus {
    Pending,
    Settled,
    Cancelled,
}

impl ReimbursementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReimbursementStatus::Pending => "pending",
            ReimbursementStatus::Settled => "settled",
            ReimbursementStatus::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for ReimbursementStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReimbursementStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(ReimbursementStatus::Pending),
            "settled" => Ok(ReimbursementStatus::Settled),
            "cancelled" => Ok(ReimbursementStatus::Cancelled),
            other => Err(format!("unknown reimbursement status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: Uuid,
    pub kind: MarketKind,
    pub esi_location_id: i64,
    /// `None` only for citadels still pending detail-fetch resolution.
    /// NPC hubs always carry `Some(_)`; the DB CHECK constraint enforces this.
    pub region_id: Option<i64>,
    pub name: Option<String>,
    pub short_label: Option<String>,
    pub is_hub: bool,
    pub is_public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPrice {
    pub market_id: Uuid,
    pub type_id: i64,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub list_id: Uuid,
    pub type_id: i64,
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
    pub requester_user_id: Uuid,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractType {
    ItemExchange,
    Auction,
    Courier,
    Unknown,
}

impl ContractType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContractType::ItemExchange => "item_exchange",
            ContractType::Auction => "auction",
            ContractType::Courier => "courier",
            ContractType::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ContractType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub fn as_str(self) -> &'static str {
        match self {
            ContractStatus::Outstanding => "outstanding",
            ContractStatus::InProgress => "in_progress",
            ContractStatus::FinishedIssuer => "finished_issuer",
            ContractStatus::FinishedContractor => "finished_contractor",
            ContractStatus::Finished => "finished",
            ContractStatus::Cancelled => "cancelled",
            ContractStatus::Rejected => "rejected",
            ContractStatus::Failed => "failed",
            ContractStatus::Deleted => "deleted",
            ContractStatus::Reversed => "reversed",
        }
    }

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

impl fmt::Display for ContractStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractMatchState {
    Pending,
    Confirmed,
    Rejected,
    Superseded,
}

impl ContractMatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            ContractMatchState::Pending => "pending",
            ContractMatchState::Confirmed => "confirmed",
            ContractMatchState::Rejected => "rejected",
            ContractMatchState::Superseded => "superseded",
        }
    }
}

impl fmt::Display for ContractMatchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ContractMatchState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(ContractMatchState::Pending),
            "confirmed" => Ok(ContractMatchState::Confirmed),
            "rejected" => Ok(ContractMatchState::Rejected),
            "superseded" => Ok(ContractMatchState::Superseded),
            other => Err(format!("unknown contract match state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: Uuid,
    pub esi_contract_id: i64,
    pub issuer_character_id: i64,
    pub issuer_user_id: Option<Uuid>,
    pub assignee_character_id: Option<i64>,
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
    pub start_location_id: Option<i64>,
    pub end_location_id: Option<i64>,
    pub items_synced_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractItem {
    pub contract_id: Uuid,
    pub record_id: i64,
    pub type_id: i32,
    pub quantity: i64,
    pub is_included: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSummary {
    pub esi_contract_id: i64,
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
    pub type_id: i64,
    pub type_name: String,
}
