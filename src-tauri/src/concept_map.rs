//! Explicit, incremental project concept map.

use std::collections::BTreeMap;

use rusqlite::Connection;
use scraper::{Html, Selector};
use serde::Serialize;
use tauri::State;

use crate::db::DbHandle;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConceptMention {
    pub id: String,
    pub thread_id: String,
    pub thread_title: String,
    pub artifact_version: i64,
    pub cfy_id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConceptNode {
    pub id: String,
    pub name: String,
    pub mentions: Vec<ConceptMention>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConceptLink {
    pub id: String,
    pub from_concept_id: String,
    pub to_concept_id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ConceptMap {
    pub concepts: Vec<ConceptNode>,
    pub links: Vec<ConceptLink>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExtractedMention {
    concept: String,
    cfy_id: String,
    kind: String,
    label: String,
}

fn canonical(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn extract(html: &str) -> Vec<ExtractedMention> {
    let document = Html::parse_document(html);
    let selector =
        Selector::parse("[data-cfy-concepts][data-cfy-id]").expect("static selector is valid");
    let mut mentions = Vec::new();
    for element in document.select(&selector) {
        let cfy_id = element.value().attr("data-cfy-id").unwrap().to_owned();
        let kind = if element.value().attr("data-cfy-next-question").is_some() {
            "question"
        } else if element.value().name() == "figure"
            || element.value().name() == "svg"
            || element.ancestors().any(|node| {
                node.value()
                    .as_element()
                    .is_some_and(|ancestor| ancestor.name() == "svg")
            })
        {
            "visual"
        } else {
            "section"
        };
        let text = element.text().collect::<Vec<_>>().join(" ");
        let label = element
            .value()
            .attr("aria-label")
            .unwrap_or(&text)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(180)
            .collect::<String>();
        for concept in element
            .value()
            .attr("data-cfy-concepts")
            .unwrap()
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .take(8)
        {
            mentions.push(ExtractedMention {
                concept: concept.chars().take(100).collect(),
                cfy_id: cfy_id.clone(),
                kind: kind.to_owned(),
                label: label.clone(),
            });
            if mentions.len() >= 500 {
                return mentions;
            }
        }
    }
    mentions
}

pub fn replace_for_artifact(
    conn: &Connection,
    project_id: &str,
    thread_id: &str,
    version: i64,
    html: &str,
) -> rusqlite::Result<()> {
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'concept_mentions')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM concept_mentions WHERE thread_id = ?1",
        [thread_id],
    )?;
    for mention in extract(html) {
        let canonical_name = canonical(&mention.concept);
        conn.execute(
            "INSERT INTO concepts (id, project_id, canonical_name, display_name)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, canonical_name) DO NOTHING",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                project_id,
                canonical_name,
                mention.concept,
            ],
        )?;
        let concept_id: String = conn.query_row(
            "SELECT id FROM concepts WHERE project_id = ?1 AND canonical_name = ?2",
            rusqlite::params![project_id, canonical_name],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO concept_mentions
                 (id, concept_id, thread_id, artifact_version, cfy_id, kind, label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                concept_id,
                thread_id,
                version,
                mention.cfy_id,
                mention.kind,
                mention.label,
            ],
        )?;
    }
    conn.execute(
        "DELETE FROM concepts
         WHERE project_id = ?1
           AND NOT EXISTS (SELECT 1 FROM concept_mentions m WHERE m.concept_id = concepts.id)
           AND NOT EXISTS (SELECT 1 FROM concept_links l WHERE l.from_concept_id = concepts.id OR l.to_concept_id = concepts.id)",
        [project_id],
    )?;
    Ok(())
}

fn read_map(conn: &Connection, project_id: &str) -> rusqlite::Result<ConceptMap> {
    let mut statement = conn.prepare(
        "SELECT c.id, c.display_name, m.id, m.thread_id, t.title,
                m.artifact_version, m.cfy_id, m.kind, m.label
         FROM concepts c
         JOIN concept_mentions m ON m.concept_id = c.id
         JOIN threads t ON t.id = m.thread_id
         WHERE c.project_id = ?1
         ORDER BY lower(c.display_name), m.rowid DESC
         LIMIT 2001",
    )?;
    let rows = statement.query_map([project_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            ConceptMention {
                id: row.get(2)?,
                thread_id: row.get(3)?,
                thread_title: row.get(4)?,
                artifact_version: row.get(5)?,
                cfy_id: row.get(6)?,
                kind: row.get(7)?,
                label: row.get(8)?,
            },
        ))
    })?;
    let mut grouped: BTreeMap<String, ConceptNode> = BTreeMap::new();
    let mut count = 0usize;
    for row in rows {
        let (id, name, mention) = row?;
        count += 1;
        if count > 2000 {
            break;
        }
        grouped
            .entry(id.clone())
            .or_insert_with(|| ConceptNode {
                id,
                name,
                mentions: Vec::new(),
            })
            .mentions
            .push(mention);
    }
    let mut link_statement = conn.prepare(
        "SELECT id, from_concept_id, to_concept_id, label
         FROM concept_links WHERE project_id = ?1 ORDER BY created_at, rowid LIMIT 1000",
    )?;
    let links = link_statement
        .query_map([project_id], |row| {
            Ok(ConceptLink {
                id: row.get(0)?,
                from_concept_id: row.get(1)?,
                to_concept_id: row.get(2)?,
                label: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let concept_count = grouped.len();
    Ok(ConceptMap {
        concepts: grouped.into_values().take(500).collect(),
        links,
        truncated: count > 2000 || concept_count > 500,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_concept_map(db: State<DbHandle>, project_id: String) -> Result<ConceptMap, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    read_map(&conn, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn pin_concept_link(
    db: State<DbHandle>,
    project_id: String,
    from_concept_id: String,
    to_concept_id: String,
    label: String,
) -> Result<(), String> {
    let label = label.trim();
    if label.is_empty() || from_concept_id == to_concept_id {
        return Err("a relationship needs two different concepts and a label".to_owned());
    }
    let conn = db.lock().map_err(|error| error.to_string())?;
    let valid: bool = conn
        .query_row(
            "SELECT COUNT(*) = 2 FROM concepts WHERE project_id = ?1 AND id IN (?2, ?3)",
            rusqlite::params![project_id, from_concept_id, to_concept_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !valid {
        return Err("both concepts must belong to this project".to_owned());
    }
    conn.execute(
        "INSERT OR IGNORE INTO concept_links
             (id, project_id, from_concept_id, to_concept_id, label)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            project_id,
            from_concept_id,
            to_concept_id,
            label.chars().take(100).collect::<String>()
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn remove_concept_link(db: State<DbHandle>, id: String) -> Result<bool, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    conn.execute("DELETE FROM concept_links WHERE id = ?1", [id])
        .map(|changed| changed > 0)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn distinguish_concept(
    db: State<DbHandle>,
    mention_id: String,
    new_name: String,
) -> Result<(), String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("a distinct concept needs a name".to_owned());
    }
    let conn = db.lock().map_err(|error| error.to_string())?;
    let project_id: String = conn
        .query_row(
            "SELECT c.project_id FROM concept_mentions m JOIN concepts c ON c.id = m.concept_id WHERE m.id = ?1",
            [&mention_id],
            |row| row.get(0),
        )
        .map_err(|_| "concept mention not found".to_owned())?;
    let canonical_name = canonical(new_name);
    conn.execute(
        "INSERT INTO concepts (id, project_id, canonical_name, display_name)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(project_id, canonical_name) DO NOTHING",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            project_id,
            canonical_name,
            new_name
        ],
    )
    .map_err(|error| error.to_string())?;
    let target: String = conn
        .query_row(
            "SELECT id FROM concepts WHERE project_id = ?1 AND canonical_name = ?2",
            rusqlite::params![project_id, canonical_name],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    conn.execute(
        "UPDATE concept_mentions SET concept_id = ?2 WHERE id = ?1",
        rusqlite::params![mention_id, target],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn merge_concepts(
    db: State<DbHandle>,
    source_concept_id: String,
    target_concept_id: String,
) -> Result<(), String> {
    if source_concept_id == target_concept_id {
        return Ok(());
    }
    let mut conn = db.lock().map_err(|error| error.to_string())?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    let same_project: bool = tx
        .query_row(
            "SELECT COUNT(DISTINCT project_id) = 1 AND COUNT(*) = 2 FROM concepts WHERE id IN (?1, ?2)",
            rusqlite::params![source_concept_id, target_concept_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !same_project {
        return Err("both concepts must exist in the same project".to_owned());
    }
    let source_links = {
        let mut statement = tx
            .prepare(
                "SELECT project_id, from_concept_id, to_concept_id, label
                 FROM concept_links WHERE from_concept_id = ?1 OR to_concept_id = ?1",
            )
            .map_err(|error| error.to_string())?;
        let links = statement
            .query_map([&source_concept_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.to_string())?;
        links
    };
    tx.execute(
        "DELETE FROM concept_mentions
         WHERE concept_id = ?1 AND EXISTS (
           SELECT 1 FROM concept_mentions target
           WHERE target.concept_id = ?2
             AND target.thread_id = concept_mentions.thread_id
             AND target.artifact_version = concept_mentions.artifact_version
             AND target.cfy_id = concept_mentions.cfy_id
         )",
        rusqlite::params![source_concept_id, target_concept_id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE concept_mentions SET concept_id = ?2 WHERE concept_id = ?1",
        rusqlite::params![source_concept_id, target_concept_id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "DELETE FROM concept_links WHERE from_concept_id = ?1 OR to_concept_id = ?1",
        [&source_concept_id],
    )
    .map_err(|error| error.to_string())?;
    for (project_id, from, to, label) in source_links {
        let new_from = if from == source_concept_id {
            &target_concept_id
        } else {
            &from
        };
        let new_to = if to == source_concept_id {
            &target_concept_id
        } else {
            &to
        };
        if new_from == new_to {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO concept_links
                 (id, project_id, from_concept_id, to_concept_id, label)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                project_id,
                new_from,
                new_to,
                label
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute("DELETE FROM concepts WHERE id = ?1", [&source_concept_id])
        .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_explicit_bounded_metadata() {
        let mentions = extract(
            r#"<h2 data-cfy-id="sec-own" data-cfy-concepts="Ownership | Borrowing">Ownership</h2>
               <figure data-cfy-id="fig-life" data-cfy-concepts="Borrowing" aria-label="Borrow lifetime"></figure>"#,
        );
        assert_eq!(mentions.len(), 3);
        assert_eq!(mentions[0].kind, "section");
        assert_eq!(mentions[2].kind, "visual");
    }

    #[test]
    fn incrementally_replaces_one_threads_evidence_and_keeps_others() {
        let path =
            std::env::temp_dir().join(format!("conceptify-concepts-{}.db", uuid::Uuid::new_v4()));
        let handle = crate::db::init_at(&path).unwrap();
        let conn = handle.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'P', '/tmp/concepts')",
            [],
        )
        .unwrap();
        for id in ["t1", "t2"] {
            conn.execute(
                "INSERT INTO threads (id, project_id, title, slug, initial_question, status)
                 VALUES (?1, 'p1', ?1, ?1, '?', 'ready')",
                [id],
            )
            .unwrap();
        }
        replace_for_artifact(
            &conn,
            "p1",
            "t1",
            1,
            r#"<h2 data-cfy-id="s1" data-cfy-concepts="Ownership|Borrowing">One</h2>"#,
        )
        .unwrap();
        replace_for_artifact(
            &conn,
            "p1",
            "t2",
            1,
            r#"<figure data-cfy-id="f1" data-cfy-concepts="Lifetimes" aria-label="Lifetime chart"></figure>"#,
        )
        .unwrap();
        replace_for_artifact(
            &conn,
            "p1",
            "t1",
            2,
            r#"<h2 data-cfy-id="s2" data-cfy-concepts="Ownership">Updated</h2>"#,
        )
        .unwrap();
        let map = read_map(&conn, "p1").unwrap();
        assert_eq!(map.concepts.len(), 2);
        let ownership = map
            .concepts
            .iter()
            .find(|node| node.name == "Ownership")
            .unwrap();
        assert_eq!(ownership.mentions[0].artifact_version, 2);
        assert_eq!(ownership.mentions[0].cfy_id, "s2");
        assert!(map.concepts.iter().any(|node| node.name == "Lifetimes"));
        assert!(!map.concepts.iter().any(|node| node.name == "Borrowing"));
        drop(conn);
        drop(handle);
        let _ = std::fs::remove_file(path);
    }
}
