//! Shared time based capture retention selection.

use chrono::{DateTime, Months, TimeDelta, Utc};
use rusqlite::Connection;

pub const POLICIES: [&str; 8] = [
    "1_day",
    "3_days",
    "7_days",
    "1_month",
    "3_months",
    "6_months",
    "12_months",
    "never",
];

pub fn is_valid_policy(policy: &str) -> bool {
    POLICIES.contains(&policy)
}

pub fn candidate_ids(
    conn: &Connection,
    policy: &str,
    policy_started_at: Option<&str>,
    at: DateTime<Utc>,
) -> Result<Vec<(String, String)>, String> {
    if !is_valid_policy(policy) {
        return Err("Política de retenção inválida.".into());
    }
    let cutoff = match policy {
        "1_day" => at.checked_sub_signed(TimeDelta::days(1)),
        "3_days" => at.checked_sub_signed(TimeDelta::days(3)),
        "7_days" => at.checked_sub_signed(TimeDelta::days(7)),
        "1_month" => at.checked_sub_months(Months::new(1)),
        "3_months" => at.checked_sub_months(Months::new(3)),
        "6_months" => at.checked_sub_months(Months::new(6)),
        "12_months" => at.checked_sub_months(Months::new(12)),
        "never" => return Ok(Vec::new()),
        _ => unreachable!(),
    }
    .ok_or("Não foi possível calcular o limite da retenção.")?
    .to_rfc3339();
    let sql = if policy_started_at.is_some_and(|value| !value.trim().is_empty()) {
        "SELECT id, captured_at FROM captures
         WHERE capture_kind <> 'reference'
           AND COALESCE(json_extract(metadata_json, '$.capture_origin'), 'explicit_hotkey') <> 'file_import'
           AND captured_at < ?1 AND captured_at >= ?2
         ORDER BY captured_at DESC, id DESC"
    } else {
        "SELECT id, captured_at FROM captures
         WHERE capture_kind <> 'reference'
           AND COALESCE(json_extract(metadata_json, '$.capture_origin'), 'explicit_hotkey') <> 'file_import'
           AND captured_at < ?1
         ORDER BY captured_at DESC, id DESC"
    };
    let mut stmt = conn.prepare(sql).map_err(err)?;
    let mut candidates = Vec::new();
    if let Some(started_at) = policy_started_at.filter(|value| !value.trim().is_empty()) {
        let rows = stmt
            .query_map(rusqlite::params![cutoff, started_at], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(err)?;
        candidates.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?);
    } else {
        let rows = stmt
            .query_map([cutoff], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(err)?;
        candidates.extend(rows.collect::<rusqlite::Result<Vec<_>>>().map_err(err)?);
    }
    Ok(candidates)
}

fn err(error: impl ToString) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{schema, storage::open_or_create};

    #[test]
    fn retention_candidates_skip_references_and_file_imports() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_or_create(dir.path(), &"b5".repeat(32)).unwrap();
        schema::migrate(&conn).unwrap();
        let old = (Utc::now() - TimeDelta::days(40)).to_rfc3339();
        for (id, kind, metadata) in [
            ("expired", "capture", "{}"),
            ("reference", "reference", "{}"),
            ("import", "capture", r#"{"capture_origin":"file_import"}"#),
        ] {
            conn.execute(
                "INSERT INTO captures (id, context_id, content_text, content_hash, captured_at, platform, metadata_json, capture_kind, created_at)
                 VALUES (?1, 'inbox', '', ?1, ?2, 'windows', ?3, ?4, ?2)",
                rusqlite::params![id, old, metadata, kind],
            ).unwrap();
        }
        let selected = candidate_ids(&conn, "1_month", None, Utc::now()).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["expired"]
        );
        assert!(candidate_ids(&conn, "never", None, Utc::now())
            .unwrap()
            .is_empty());
        assert!(candidate_ids(&conn, "invalid", None, Utc::now()).is_err());
    }
}
