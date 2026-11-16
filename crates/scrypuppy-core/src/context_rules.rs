//! Apply existing automatic Context rules to a saved capture.
use std::collections::HashMap;

use regex::RegexBuilder;
use rusqlite::{params, Connection};

use crate::{library::get_capture_by_id, search_index};

#[derive(Clone, Debug)]
struct Condition {
    field: String,
    operator: String,
    value: String,
}

#[derive(Clone, Debug)]
struct Rule {
    context_id: String,
    match_mode: String,
    conditions: Vec<Condition>,
}

/// Apply enabled rules in the same database and refresh the capture's search row.
pub fn apply_enabled_rules(conn: &Connection, capture_id: &str) -> Result<Vec<String>, String> {
    let capture = get_capture_by_id(conn, capture_id)?;
    let mut stmt = conn
        .prepare(
            "SELECT r.id, r.context_id, r.match_mode, c.field, c.operator, c.value
             FROM context_rules r
             JOIN context_rule_conditions c ON c.rule_id = r.id
             WHERE r.enabled = 1
             ORDER BY r.id, c.ordinal",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                Condition {
                    field: row.get(3)?,
                    operator: row.get(4)?,
                    value: row.get(5)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);

    let mut rules = HashMap::<String, Rule>::new();
    for (rule_id, context_id, match_mode, condition) in rows {
        let rule = rules.entry(rule_id).or_insert_with(|| Rule {
            context_id,
            match_mode,
            conditions: Vec::new(),
        });
        rule.conditions.push(condition);
    }

    let at = chrono::Utc::now().to_rfc3339();
    let mut assigned = Vec::new();
    for rule in rules.values() {
        let matched = matches_rule(
            &capture,
            &rule
                .conditions
                .iter()
                .map(|condition| {
                    (
                        condition.field.clone(),
                        condition.operator.clone(),
                        condition.value.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            &rule.match_mode,
        );
        if matched
            && conn
                .execute(
                    "INSERT OR IGNORE INTO capture_contexts
                     (capture_id, context_id, assignment_origin, confidence, created_at)
                     VALUES (?1, ?2, 'automatic', 1.0, ?3)",
                    params![capture_id, rule.context_id, at],
                )
                .map_err(|error| error.to_string())?
                > 0
        {
            assigned.push(rule.context_id.clone());
        }
    }
    assigned.sort();
    assigned.dedup();
    if !assigned.is_empty() {
        search_index::refresh_search_document(conn, capture_id, &at)?;
    }
    Ok(assigned)
}

/// Evaluate a draft rule with the same matcher used for stored rules.
pub fn matches_rule(
    capture: &crate::library::CaptureDto,
    conditions: &[(String, String, String)],
    match_mode: &str,
) -> bool {
    let conditions = conditions
        .iter()
        .map(|(field, operator, value)| Condition {
            field: field.clone(),
            operator: operator.clone(),
            value: value.clone(),
        })
        .collect::<Vec<_>>();
    if match_mode == "any" {
        !conditions.is_empty()
            && conditions
                .iter()
                .any(|condition| condition_matches(capture, condition))
    } else {
        !conditions.is_empty()
            && conditions
                .iter()
                .all(|condition| condition_matches(capture, condition))
    }
}

fn condition_matches(capture: &crate::library::CaptureDto, condition: &Condition) -> bool {
    let expected = condition.value.trim();
    let candidates = match condition.field.as_str() {
        "application" => [
            capture.source_app_name.as_deref(),
            capture.source_app_id.as_deref(),
            capture.source_process_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::to_string)
        .collect::<Vec<_>>(),
        "content_type" => content_types(capture),
        "window_title" => capture.window_title.iter().cloned().collect(),
        "file_extension" => capture
            .files
            .iter()
            .filter_map(|file| file.extension.as_deref())
            .map(|value| value.trim_start_matches('.').to_string())
            .collect(),
        "file_path" => capture
            .files
            .iter()
            .flat_map(|file| [file.original_path.as_deref(), file.local_path.as_deref()])
            .flatten()
            .map(str::to_string)
            .collect(),
        "text" => {
            let mut values = vec![capture.content_text.clone()];
            values.extend(
                capture
                    .representations
                    .iter()
                    .filter_map(|representation| representation.text_content.clone()),
            );
            values.extend(capture.files.iter().map(|file| file.display_name.clone()));
            values.extend(capture.tags.iter().cloned());
            values.extend(capture.entities.iter().map(|entity| entity.value.clone()));
            if let Some(text) = capture.ocr.as_ref().and_then(|ocr| ocr.text.clone()) {
                values.push(text);
            }
            values
        }
        _ => Vec::new(),
    };
    match condition.operator.as_str() {
        "equals" => candidates
            .iter()
            .any(|value| value.trim().eq_ignore_ascii_case(expected)),
        "contains" => {
            let expected = expected.to_lowercase();
            candidates
                .iter()
                .any(|value| value.to_lowercase().contains(&expected))
        }
        "matches" => RegexBuilder::new(expected)
            .case_insensitive(true)
            .build()
            .is_ok_and(|regex| candidates.iter().any(|value| regex.is_match(value))),
        _ => false,
    }
}

fn content_types(capture: &crate::library::CaptureDto) -> Vec<String> {
    if capture.content_kind == "files" {
        let mut values = vec!["files".to_string()];
        if capture
            .files
            .iter()
            .any(|file| file.entry_kind == "application")
        {
            values.push("application".into());
        }
        if capture
            .files
            .iter()
            .any(|file| file.entry_kind == "directory")
        {
            values.push("folder".into());
        }
        if capture.files.iter().any(|file| {
            matches!(
                file.entry_kind.as_str(),
                "file" | "shortcut" | "virtual_file"
            )
        }) || capture.files.is_empty()
        {
            values.push("file".into());
        }
        return values;
    }
    match capture.content_kind.as_str() {
        "image" => vec!["image".into()],
        "url" => vec!["link".into()],
        "html" | "rich_text" => vec!["formatted_text".into(), "text".into()],
        _ => vec!["text".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_enabled_rules, matches_rule};
    use crate::{
        capture_store::save_clipboard_capture,
        clipboard::{ClipboardRepresentation, ClipboardSnapshot},
        library::get_capture_by_id,
        schema::migrate,
    };
    use rusqlite::Connection;

    #[test]
    fn applies_composed_enabled_rules_and_refreshes_search_index() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
             VALUES ('work', 'Work', 'work', 'work', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO context_rules (id, context_id, name, enabled, match_mode, created_at, updated_at)
             VALUES ('rule', 'work', 'Launch notes', 1, 'all', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO context_rule_conditions (id, rule_id, ordinal, field, operator, value, created_at)
             VALUES ('condition-1', 'rule', 0, 'text', 'matches', 'launch.*notes', 'now');
             INSERT INTO context_rule_conditions (id, rule_id, ordinal, field, operator, value, created_at)
             VALUES ('condition-2', 'rule', 1, 'content_type', 'equals', 'text', 'now');",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let capture_id = save_clipboard_capture(
            &mut conn,
            dir.path(),
            &ClipboardSnapshot {
                representations: vec![ClipboardRepresentation::PlainText(
                    "Launch meeting notes".into(),
                )],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();

        let assigned = apply_enabled_rules(&conn, &capture_id).unwrap();

        assert_eq!(assigned, ["work"]);
        let capture = get_capture_by_id(&conn, &capture_id).unwrap();
        assert!(matches_rule(
            &capture,
            &[
                ("text".into(), "matches".into(), "launch.*notes".into()),
                ("content_type".into(), "equals".into(), "text".into()),
            ],
            "all",
        ));
        assert_eq!(capture.contexts[0].assignment_origin, "automatic");
        let indexed_metadata: String = conn
            .query_row(
                "SELECT metadata FROM capture_search_documents WHERE capture_id = ?",
                [&capture_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(indexed_metadata.contains("Work"));
    }
}
