//! Principal resolution — maps ESI contract fields to `Principal` rows.
//!
//! Called by both the character and corp contract pollers. The result drives
//! contract matching (which reimbursements to compare) and authz (who can
//! confirm/settle).

use crate::{EsiCharacterId, EsiCorporationId, Principal, PrincipalIndex};
use uuid::Uuid;

/// Resolved issuer/assignee for one ESI contract.
#[derive(Debug, Clone)]
pub struct ResolvedParties {
    pub issuer_principal_id: Option<Uuid>,
    pub assignee_principal_id: Option<Uuid>,
    /// True when assignee was present in ESI but could not be resolved to any
    /// known user or corp. Surface in the needs-attention tray.
    pub assignee_unknown: bool,
}

/// Raw fields from `nea_esi::EsiContract` that we need for resolution.
/// Extracted here so the domain crate stays free of the nea-esi dependency.
/// `assignee_id` is polymorphic in ESI — it's either a character id or a corp
/// id depending on the contract. The resolver tries both lookups, so we model
/// it as a raw `i64` here rather than committing to one newtype.
#[derive(Debug, Clone)]
pub struct EsiContractParties {
    pub issuer_id: EsiCharacterId,
    pub issuer_corporation_id: EsiCorporationId,
    /// `for_corporation` field from ESI. When true, the issuer side is the corp.
    pub for_corporation: bool,
    pub assignee_id: Option<i64>,
}

/// Resolve issuer and assignee ESI ids into `Principal` ids using the provided
/// in-memory index. Tests cover all combinations.
pub fn resolve_contract_parties(c: &EsiContractParties, idx: &PrincipalIndex) -> ResolvedParties {
    // Issuer side.
    let issuer_principal_id: Option<Uuid> = if c.for_corporation {
        // Corp contract: issuer is the corporation.
        idx.corp_by_esi_id
            .get(&c.issuer_corporation_id)
            .and_then(|corp_id| idx.principal_by_corp_id.get(corp_id))
            .map(|p| p.id)
    } else {
        // Personal contract: issuer is a character, look up the user.
        idx.user_by_character_id
            .get(&c.issuer_id)
            .and_then(|user_id| idx.principal_by_user_id.get(user_id))
            .map(|p| p.id)
    };

    // Assignee side. `assignee_id` is polymorphic in ESI: try corp first,
    // then character, then surface as unknown.
    let (assignee_principal_id, assignee_unknown) = match c.assignee_id {
        None => (None, false), // Public contract — matcher skips these anyway.
        Some(esi_id) => {
            if let Some(corp_id) = idx.corp_by_esi_id.get(&EsiCorporationId(esi_id)) {
                let pid = idx.principal_by_corp_id.get(corp_id).map(|p| p.id);
                (pid, pid.is_none())
            } else if let Some(user_id) = idx.user_by_character_id.get(&EsiCharacterId(esi_id)) {
                let pid = idx.principal_by_user_id.get(user_id).map(|p| p.id);
                (pid, pid.is_none())
            } else {
                (None, true)
            }
        }
    };

    ResolvedParties {
        issuer_principal_id,
        assignee_principal_id,
        assignee_unknown,
    }
}

// ── Helper constructors for the index (used by tests and workers) ─────────────

impl PrincipalIndex {
    /// Insert a user-principal mapping (convenience for tests).
    pub fn add_user(&mut self, character_id: EsiCharacterId, user_id: Uuid, principal: Principal) {
        self.user_by_character_id.insert(character_id, user_id);
        self.principal_by_user_id.insert(user_id, principal);
    }

    /// Insert a corp-principal mapping (convenience for tests).
    pub fn add_corp(&mut self, esi_corp_id: EsiCorporationId, corp_id: Uuid, principal: Principal) {
        self.corp_by_esi_id.insert(esi_corp_id, corp_id);
        self.principal_by_corp_id.insert(corp_id, principal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrincipalKind;
    use uuid::Uuid;

    fn make_user_principal(user_id: Uuid) -> Principal {
        Principal {
            id: Uuid::new_v4(),
            kind: PrincipalKind::User,
            user_id: Some(user_id),
            corp_id: None,
        }
    }

    fn make_corp_principal(corp_id: Uuid) -> Principal {
        Principal {
            id: Uuid::new_v4(),
            kind: PrincipalKind::Corp,
            user_id: None,
            corp_id: Some(corp_id),
        }
    }

    /// Personal contract: issuer_character → user-principal, assignee_character → user-principal.
    #[test]
    fn personal_user_to_user() {
        let mut idx = PrincipalIndex::default();

        let issuer_char = EsiCharacterId(1001);
        let issuer_user = Uuid::new_v4();
        let issuer_p = make_user_principal(issuer_user);

        let assignee_char = EsiCharacterId(2002);
        let assignee_user = Uuid::new_v4();
        let assignee_p = make_user_principal(assignee_user);

        idx.add_user(issuer_char, issuer_user, issuer_p.clone());
        idx.add_user(assignee_char, assignee_user, assignee_p.clone());

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: issuer_char,
                issuer_corporation_id: EsiCorporationId(9001),
                for_corporation: false,
                assignee_id: Some(assignee_char.get()),
            },
            &idx,
        );

        assert_eq!(parties.issuer_principal_id, Some(issuer_p.id));
        assert_eq!(parties.assignee_principal_id, Some(assignee_p.id));
        assert!(!parties.assignee_unknown);
    }

    /// Corp contract (for_corporation=true): issuer side is the corp.
    #[test]
    fn corp_issuer_user_assignee() {
        let mut idx = PrincipalIndex::default();

        let esi_corp_id = EsiCorporationId(9001);
        let corp_id = Uuid::new_v4();
        let corp_p = make_corp_principal(corp_id);
        idx.add_corp(esi_corp_id, corp_id, corp_p.clone());

        let assignee_char = EsiCharacterId(2002);
        let assignee_user = Uuid::new_v4();
        let assignee_p = make_user_principal(assignee_user);
        idx.add_user(assignee_char, assignee_user, assignee_p.clone());

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: EsiCharacterId(1001),
                issuer_corporation_id: esi_corp_id,
                for_corporation: true,
                assignee_id: Some(assignee_char.get()),
            },
            &idx,
        );

        assert_eq!(parties.issuer_principal_id, Some(corp_p.id));
        assert_eq!(parties.assignee_principal_id, Some(assignee_p.id));
        assert!(!parties.assignee_unknown);
    }

    /// Assignee is a known corp.
    #[test]
    fn user_issuer_corp_assignee() {
        let mut idx = PrincipalIndex::default();

        let issuer_char = EsiCharacterId(1001);
        let issuer_user = Uuid::new_v4();
        let issuer_p = make_user_principal(issuer_user);
        idx.add_user(issuer_char, issuer_user, issuer_p.clone());

        let esi_corp_id = EsiCorporationId(9002);
        let corp_id = Uuid::new_v4();
        let corp_p = make_corp_principal(corp_id);
        idx.add_corp(esi_corp_id, corp_id, corp_p.clone());

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: issuer_char,
                issuer_corporation_id: EsiCorporationId(9001),
                for_corporation: false,
                // EVE encodes assignee_id as the corp's EVE id.
                assignee_id: Some(esi_corp_id.get()),
            },
            &idx,
        );

        assert_eq!(parties.issuer_principal_id, Some(issuer_p.id));
        assert_eq!(parties.assignee_principal_id, Some(corp_p.id));
        assert!(!parties.assignee_unknown);
    }

    /// Corp-to-corp contract.
    #[test]
    fn corp_to_corp() {
        let mut idx = PrincipalIndex::default();

        let issuer_corp_esi = EsiCorporationId(9001);
        let issuer_corp_id = Uuid::new_v4();
        let issuer_corp_p = make_corp_principal(issuer_corp_id);
        idx.add_corp(issuer_corp_esi, issuer_corp_id, issuer_corp_p.clone());

        let assignee_corp_esi = EsiCorporationId(9002);
        let assignee_corp_id = Uuid::new_v4();
        let assignee_corp_p = make_corp_principal(assignee_corp_id);
        idx.add_corp(assignee_corp_esi, assignee_corp_id, assignee_corp_p.clone());

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: EsiCharacterId(1001),
                issuer_corporation_id: issuer_corp_esi,
                for_corporation: true,
                assignee_id: Some(assignee_corp_esi.get()),
            },
            &idx,
        );

        assert_eq!(parties.issuer_principal_id, Some(issuer_corp_p.id));
        assert_eq!(parties.assignee_principal_id, Some(assignee_corp_p.id));
        assert!(!parties.assignee_unknown);
    }

    /// Assignee id is not in our index — should be flagged as unknown.
    #[test]
    fn assignee_unknown() {
        let mut idx = PrincipalIndex::default();

        let issuer_char = EsiCharacterId(1001);
        let issuer_user = Uuid::new_v4();
        let issuer_p = make_user_principal(issuer_user);
        idx.add_user(issuer_char, issuer_user, issuer_p.clone());

        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: issuer_char,
                issuer_corporation_id: EsiCorporationId(9001),
                for_corporation: false,
                assignee_id: Some(99999),
            },
            &idx,
        );

        assert_eq!(parties.issuer_principal_id, Some(issuer_p.id));
        assert_eq!(parties.assignee_principal_id, None);
        assert!(parties.assignee_unknown);
    }

    /// Public contract (no assignee): both fields are None, unknown=false.
    #[test]
    fn public_contract_no_assignee() {
        let idx = PrincipalIndex::default();
        let parties = resolve_contract_parties(
            &EsiContractParties {
                issuer_id: EsiCharacterId(1001),
                issuer_corporation_id: EsiCorporationId(9001),
                for_corporation: false,
                assignee_id: None,
            },
            &idx,
        );
        assert_eq!(parties.assignee_principal_id, None);
        assert!(!parties.assignee_unknown);
    }
}
