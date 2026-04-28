//! Resolve item-name strings to ESI type ids, with a Postgres-backed cache.

use std::collections::{HashMap, HashSet};

pub use domain::multibuy::name_key as normalize_key;
use domain::ResolvedType;
use nea_esi::EsiClient;
use sqlx::PgPool;

/// Returns `(resolved-by-key, unresolved-original-names)`. The map's keys are
/// the normalized `name_key` values, so callers must normalize their lookups
/// before indexing into the result.
pub async fn resolve_type_ids(
    pool: &PgPool,
    esi: &EsiClient,
    names: &[String],
) -> anyhow::Result<(HashMap<String, ResolvedType>, Vec<String>)> {
    if names.is_empty() {
        return Ok((HashMap::new(), Vec::new()));
    }

    // Dedup by normalized key; keep first-seen casing for fallback display.
    let mut requested: Vec<(String, String)> = Vec::with_capacity(names.len());
    let mut seen: HashSet<String> = HashSet::new();
    for n in names {
        let key = normalize_key(n);
        if key.is_empty() {
            continue;
        }
        if seen.insert(key.clone()) {
            requested.push((key, n.trim().replace('\u{00A0}', " ")));
        }
    }
    if requested.is_empty() {
        return Ok((HashMap::new(), Vec::new()));
    }

    let keys: Vec<String> = requested.iter().map(|(k, _)| k.clone()).collect();

    let cached: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT name_key, name, type_id, type_name \
         FROM type_cache WHERE name_key = ANY($1::text[])",
    )
    .bind(&keys)
    .fetch_all(pool)
    .await?;

    let mut resolved: HashMap<String, ResolvedType> = HashMap::new();
    let mut have_keys: HashSet<String> = HashSet::new();
    for (k, _name, type_id, type_name) in cached {
        have_keys.insert(k.clone());
        resolved.insert(k, ResolvedType { type_id, type_name });
    }

    // Anything not in cache: ask ESI. Send the *original casing* so ESI can
    // echo it back; we lowercase locally for the cache key.
    let to_fetch: Vec<String> = requested
        .iter()
        .filter(|(k, _)| !have_keys.contains(k))
        .map(|(_, original)| original.clone())
        .collect();

    let mut unresolved: Vec<String> = Vec::new();

    if !to_fetch.is_empty() {
        let mut echoed: HashMap<String, (i64, String)> = HashMap::new();
        // nea-esi's resolve_ids already chunks at 500 (RESOLVE_IDS_CHUNK_SIZE).
        let result = esi.resolve_ids(&to_fetch).await?;
        for entry in result.inventory_types {
            let k = normalize_key(&entry.name);
            echoed.insert(k, (entry.id, entry.name));
        }

        let mut to_upsert: Vec<(String, String, i64, String)> = Vec::new();
        for (k, original) in &requested {
            if have_keys.contains(k) {
                continue;
            }
            match echoed.get(k) {
                Some((id, canonical_name)) => {
                    to_upsert.push((
                        k.clone(),
                        canonical_name.clone(),
                        *id,
                        canonical_name.clone(),
                    ));
                    resolved.insert(
                        k.clone(),
                        ResolvedType {
                            type_id: *id,
                            type_name: canonical_name.clone(),
                        },
                    );
                }
                None => unresolved.push(original.clone()),
            }
        }

        if !to_upsert.is_empty() {
            // Upsert in a single statement using UNNEST to keep the round-trip
            // count low.
            let keys: Vec<String> = to_upsert.iter().map(|(k, _, _, _)| k.clone()).collect();
            let names: Vec<String> = to_upsert.iter().map(|(_, n, _, _)| n.clone()).collect();
            let ids: Vec<i64> = to_upsert.iter().map(|(_, _, id, _)| *id).collect();
            let tnames: Vec<String> = to_upsert.iter().map(|(_, _, _, t)| t.clone()).collect();
            sqlx::query(
                r#"
                INSERT INTO type_cache (name_key, name, type_id, type_name)
                SELECT * FROM UNNEST($1::text[], $2::text[], $3::bigint[], $4::text[])
                ON CONFLICT (name_key) DO UPDATE SET
                    name      = EXCLUDED.name,
                    type_id   = EXCLUDED.type_id,
                    type_name = EXCLUDED.type_name,
                    cached_at = now()
                "#,
            )
            .bind(&keys)
            .bind(&names)
            .bind(&ids)
            .bind(&tnames)
            .execute(pool)
            .await?;
        }
    }

    Ok((resolved, unresolved))
}
