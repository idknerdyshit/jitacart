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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
