//! Semantic comparison of parallel explanations and immutable synthesis lineage.

use std::collections::BTreeSet;
use std::fs;

use rusqlite::{Connection, OptionalExtension};
use scraper::{Html, Node, Selector};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::DbHandle;
use crate::skill_catalog::ResponseIntentInput;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SynthesisSource {
    pub thread_id: String,
    pub cfy_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ComparisonSection {
    pub cfy_id: String,
    pub label: String,
    pub excerpt: String,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ComparedThread {
    pub thread_id: String,
    pub title: String,
    pub question: String,
    pub artifact_version: i64,
    pub profile: Option<ResponseIntentInput>,
    pub sections: Vec<ComparisonSection>,
    pub concepts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ThreadComparison {
    pub threads: Vec<ComparedThread>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SynthesisLineage {
    pub thread_id: String,
    pub instruction: String,
    pub sources: Vec<SynthesisSource>,
}

fn normalized_text(value: &str, max: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn section_role(id: &str, label: &str) -> String {
    let value = format!("{id} {label}").to_lowercase();
    if [
        "assumption",
        "mental-model",
        "mental model",
        "overview",
        "orientation",
    ]
    .iter()
    .any(|term| value.contains(term))
    {
        "assumption".to_owned()
    } else if ["summary", "takeaway", "conclusion", "remember"]
        .iter()
        .any(|term| value.contains(term))
    {
        "conclusion".to_owned()
    } else {
        "explanation".to_owned()
    }
}

fn sections(html: &str) -> Vec<ComparisonSection> {
    let document = Html::parse_document(html);
    let selector =
        Selector::parse("h1[data-cfy-id], h2[data-cfy-id], h3[data-cfy-id], h4[data-cfy-id]")
            .expect("static selector is valid");
    document
        .select(&selector)
        .take(40)
        .map(|heading| {
            let id = heading.value().attr("data-cfy-id").unwrap().to_owned();
            let label = normalized_text(&heading.text().collect::<Vec<_>>().join(" "), 160);
            let mut body = String::new();
            for sibling in heading.next_siblings() {
                if matches!(sibling.value(), Node::Element(element) if matches!(element.name(), "h1" | "h2" | "h3" | "h4")) {
                    break;
                }
                for descendant in sibling.descendants() {
                    if let Node::Text(text) = descendant.value() {
                        body.push_str(text);
                        body.push(' ');
                    }
                }
                if body.len() >= 900 {
                    break;
                }
            }
            ComparisonSection {
                cfy_id: id.clone(),
                role: section_role(&id, &label),
                label,
                excerpt: normalized_text(&body, 700),
            }
        })
        .collect()
}

fn compare(
    conn: &Connection,
    project_id: &str,
    thread_ids: &[String],
) -> Result<ThreadComparison, String> {
    let unique: BTreeSet<&str> = thread_ids.iter().map(String::as_str).collect();
    if unique.len() < 2 || unique.len() > 4 {
        return Err("choose between two and four distinct threads".to_owned());
    }
    let mut compared = Vec::new();
    for thread_id in unique {
        let row: Option<(String, String, i64, String, Option<String>)> = conn
            .query_row(
                "SELECT t.title, t.initial_question, a.version, a.file_path, a.response_intent_json
                 FROM threads t JOIN artifacts a ON a.thread_id = t.id
                 WHERE t.id = ?1 AND t.project_id = ?2
                 ORDER BY a.version DESC LIMIT 1",
                rusqlite::params![thread_id, project_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some((title, question, version, path, profile_json)) = row else {
            return Err(format!(
                "thread {thread_id} has no comparable artifact in this project"
            ));
        };
        let html = fs::read_to_string(&path).map_err(|error| format!("read {title}: {error}"))?;
        let concepts = {
            let mut statement = conn
                .prepare(
                    "SELECT DISTINCT c.display_name FROM concept_mentions m
                     JOIN concepts c ON c.id = m.concept_id
                     WHERE m.thread_id = ?1 ORDER BY lower(c.display_name) LIMIT 100",
                )
                .map_err(|error| error.to_string())?;
            let values = statement
                .query_map([thread_id], |row| row.get(0))
                .map_err(|error| error.to_string())?
                .collect::<rusqlite::Result<Vec<String>>>()
                .map_err(|error| error.to_string())?;
            values
        };
        compared.push(ComparedThread {
            thread_id: thread_id.to_owned(),
            title,
            question,
            artifact_version: version,
            profile: profile_json
                .as_deref()
                .and_then(|json| serde_json::from_str(json).ok()),
            sections: sections(&html),
            concepts,
        });
    }
    let mut warnings = Vec::new();
    let tagged: Vec<BTreeSet<String>> = compared
        .iter()
        .map(|thread| {
            thread
                .concepts
                .iter()
                .map(|name| name.to_lowercase())
                .collect()
        })
        .collect();
    if tagged.iter().all(|set| !set.is_empty()) {
        let shared = tagged[1..].iter().fold(tagged[0].clone(), |acc, set| {
            acc.intersection(set).cloned().collect()
        });
        if shared.is_empty() {
            warnings.push("These threads share no explicit concepts. They may cover mismatched topics; verify the selected sections before synthesis.".to_owned());
        }
    } else {
        warnings.push("Some threads have no explicit concept metadata, so topic compatibility is uncertain. Review the source questions and selected sections.".to_owned());
    }
    Ok(ThreadComparison {
        threads: compared,
        warnings,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn compare_threads(
    db: State<DbHandle>,
    project_id: String,
    thread_ids: Vec<String>,
) -> Result<ThreadComparison, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    compare(&conn, &project_id, &thread_ids)
}

#[tauri::command(rename_all = "snake_case")]
pub fn record_thread_synthesis(
    db: State<DbHandle>,
    project_id: String,
    thread_id: String,
    sources: Vec<SynthesisSource>,
    instruction: String,
) -> Result<(), String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    record(&conn, &project_id, &thread_id, &sources, &instruction)
}

fn record(
    conn: &Connection,
    project_id: &str,
    thread_id: &str,
    sources: &[SynthesisSource],
    instruction: &str,
) -> Result<(), String> {
    if sources.len() < 2
        || sources.len() > 4
        || sources.iter().any(|source| source.cfy_ids.is_empty())
    {
        return Err("a synthesis needs selected sections from two to four sources".to_owned());
    }
    let destination_ok: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1 AND project_id = ?2)",
            rusqlite::params![thread_id, project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !destination_ok {
        return Err("synthesis destination is not in this project".to_owned());
    }
    for source in sources {
        let valid: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1 AND project_id = ?2)",
                rusqlite::params![source.thread_id, project_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !valid || source.thread_id == thread_id {
            return Err(
                "every synthesis source must be a different thread in the same project".to_owned(),
            );
        }
    }
    conn.execute(
        "INSERT INTO thread_syntheses (id, project_id, thread_id, sources_json, instruction)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            project_id,
            thread_id,
            serde_json::to_string(&sources).map_err(|error| error.to_string())?,
            normalized_text(instruction, 500),
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_thread_synthesis(
    db: State<DbHandle>,
    thread_id: String,
) -> Result<Option<SynthesisLineage>, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    lineage(&conn, &thread_id)
}

fn lineage(conn: &Connection, thread_id: &str) -> Result<Option<SynthesisLineage>, String> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT sources_json, instruction FROM thread_syntheses WHERE thread_id = ?1",
            [&thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    row.map(|(json, instruction)| {
        Ok(SynthesisLineage {
            thread_id: thread_id.to_owned(),
            instruction,
            sources: serde_json::from_str(&json).map_err(|error| error.to_string())?,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_semantic_sections_and_roles() {
        let result = sections(
            r#"<article><h2 data-cfy-id="sec-mental-model">Mental model</h2><p>Assume one owner.</p><h2 data-cfy-id="sec-takeaway">Takeaway</h2><p>Borrowing is temporary.</p></article>"#,
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "assumption");
        assert!(result[0].excerpt.contains("Assume one owner"));
        assert_eq!(result[1].role, "conclusion");
    }

    #[test]
    fn compares_profiles_concepts_and_records_separate_lineage() {
        let path =
            std::env::temp_dir().join(format!("conceptify-synthesis-{}.db", uuid::Uuid::new_v4()));
        let handle = crate::db::init_at(&path).unwrap();
        let conn = handle.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'P', '/tmp/syn')",
            [],
        )
        .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "conceptify-synthesis-files-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let profile = r#"{"version":1,"depth":"balanced","language":"familiar","visuals":"auto","shape":"auto","visual_purpose":"auto"}"#;
        for (id, title, body) in [
            (
                "a",
                "Ownership basics",
                "<h2 data-cfy-id=\"sec-model\">Mental model</h2><p>One owner.</p>",
            ),
            (
                "b",
                "Borrowing view",
                "<h2 data-cfy-id=\"sec-summary\">Summary</h2><p>Loans are temporary.</p>",
            ),
            (
                "dest",
                "Synthesis",
                "<h2 data-cfy-id=\"sec-s\">Synthesis</h2>",
            ),
        ] {
            conn.execute("INSERT INTO threads (id, project_id, title, slug, initial_question, status) VALUES (?1, 'p1', ?2, ?1, '?', 'ready')", rusqlite::params![id, title]).unwrap();
            let file = dir.join(format!("{id}.html"));
            std::fs::write(&file, format!("<article>{body}</article>")).unwrap();
            conn.execute("INSERT INTO artifacts (id, thread_id, version, file_path, created_by, response_intent_json) VALUES (?1, ?2, 1, ?3, 'initial', ?4)", rusqlite::params![format!("art-{id}"), id, file.to_string_lossy(), profile]).unwrap();
        }
        conn.execute("INSERT INTO concepts (id, project_id, canonical_name, display_name) VALUES ('c1', 'p1', 'ownership', 'Ownership')", []).unwrap();
        for thread in ["a", "b"] {
            conn.execute("INSERT INTO concept_mentions (id, concept_id, thread_id, artifact_version, cfy_id, kind, label) VALUES (?1, 'c1', ?2, 1, 'sec', 'section', 'Ownership')", rusqlite::params![format!("m-{thread}"), thread]).unwrap();
        }
        let result = compare(&conn, "p1", &["a".to_owned(), "b".to_owned()]).unwrap();
        assert!(result.warnings.is_empty());
        assert_eq!(
            result.threads[0].profile.as_ref().unwrap().depth,
            "balanced"
        );
        assert!(result
            .threads
            .iter()
            .any(|thread| thread.sections[0].role == "assumption"));
        conn.execute("DELETE FROM concept_mentions WHERE thread_id = 'b'", [])
            .unwrap();
        conn.execute("INSERT INTO concepts (id, project_id, canonical_name, display_name) VALUES ('c2', 'p1', 'async', 'Async')", []).unwrap();
        conn.execute("INSERT INTO concept_mentions (id, concept_id, thread_id, artifact_version, cfy_id, kind, label) VALUES ('m-b2', 'c2', 'b', 1, 'sec', 'section', 'Async')", []).unwrap();
        let mismatch = compare(&conn, "p1", &["a".to_owned(), "b".to_owned()]).unwrap();
        assert!(mismatch.warnings[0].contains("mismatched topics"));
        let sources = vec![
            SynthesisSource {
                thread_id: "a".to_owned(),
                cfy_ids: vec!["sec-model".to_owned()],
            },
            SynthesisSource {
                thread_id: "b".to_owned(),
                cfy_ids: vec!["sec-summary".to_owned()],
            },
        ];
        record(&conn, "p1", "dest", &sources, "Reconcile them").unwrap();
        let saved = lineage(&conn, "dest").unwrap().unwrap();
        assert_eq!(saved.sources, sources);
        assert_eq!(saved.instruction, "Reconcile them");
        drop(conn);
        drop(handle);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }
}
