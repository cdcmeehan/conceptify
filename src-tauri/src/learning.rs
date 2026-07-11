//! Durable next-question suggestions and source→thread learning trails.
//!
//! Authors embed a few semantic branch prompts in an artifact. Saving extracts
//! those prompts into SQLite, where the artifact and project home can reuse,
//! edit, dismiss, and launch them without an extra model run.

use rusqlite::{Connection, OptionalExtension};
use scraper::{Html, Selector};
use serde::Serialize;
use tauri::State;

use crate::db::DbHandle;

const BRANCHES: [&str; 5] = [
    "example",
    "counterexample",
    "mechanism",
    "tradeoff",
    "prerequisite",
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LearningSuggestion {
    pub id: String,
    pub project_id: String,
    pub source_thread_id: String,
    pub source_thread_title: String,
    pub source_artifact_version: i64,
    pub source_cfy_id: String,
    pub branch: String,
    pub question: String,
    pub reason: String,
    pub status: String,
    pub launched_thread_id: Option<String>,
    pub edited_question: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LearningTrail {
    pub suggestion_id: String,
    pub source_thread_id: String,
    pub source_thread_title: String,
    pub source_artifact_version: i64,
    pub source_cfy_id: String,
    pub branch: String,
    pub question: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExtractedSuggestion {
    source_cfy_id: String,
    branch: String,
    question: String,
    reason: String,
}

fn extract(html: &str) -> Vec<ExtractedSuggestion> {
    let document = Html::parse_document(html);
    let selector =
        Selector::parse("[data-cfy-next-question][data-cfy-id]").expect("static selector is valid");
    document
        .select(&selector)
        .filter_map(|element| {
            let question = element
                .value()
                .attr("data-cfy-next-question")?
                .trim()
                .chars()
                .take(500)
                .collect::<String>();
            if question.is_empty() {
                return None;
            }
            let branch = element
                .value()
                .attr("data-cfy-branch")
                .unwrap_or("mechanism");
            if !BRANCHES.contains(&branch) {
                return None;
            }
            let reason = element
                .value()
                .attr("data-cfy-reason")
                .unwrap_or("Builds on this explanation.")
                .trim()
                .chars()
                .take(300)
                .collect::<String>();
            Some(ExtractedSuggestion {
                source_cfy_id: element.value().attr("data-cfy-id")?.to_owned(),
                branch: branch.to_owned(),
                question,
                reason,
            })
        })
        .take(8)
        .collect()
}

pub fn replace_for_artifact(
    conn: &Connection,
    project_id: &str,
    thread_id: &str,
    version: i64,
    html: &str,
) -> rusqlite::Result<()> {
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'learning_suggestions')",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(());
    }
    conn.execute(
        "UPDATE learning_suggestions SET status = 'superseded'
         WHERE source_thread_id = ?1 AND status = 'active'",
        [thread_id],
    )?;
    for suggestion in extract(html) {
        conn.execute(
            "INSERT INTO learning_suggestions
                 (id, project_id, source_thread_id, source_artifact_version,
                  source_cfy_id, branch, question, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                project_id,
                thread_id,
                version,
                suggestion.source_cfy_id,
                suggestion.branch,
                suggestion.question,
                suggestion.reason,
            ],
        )?;
    }
    Ok(())
}

fn list(conn: &Connection, project_id: &str) -> rusqlite::Result<Vec<LearningSuggestion>> {
    let mut statement = conn.prepare(
        "SELECT s.id, s.project_id, s.source_thread_id, t.title,
                s.source_artifact_version, s.source_cfy_id, s.branch,
                s.question, s.reason, s.status, s.launched_thread_id,
                s.edited_question
         FROM learning_suggestions s
         JOIN threads t ON t.id = s.source_thread_id
         WHERE s.project_id = ?1 AND s.status = 'active'
         ORDER BY s.created_at DESC, s.rowid DESC",
    )?;
    let rows = statement.query_map([project_id], |row| {
        Ok(LearningSuggestion {
            id: row.get(0)?,
            project_id: row.get(1)?,
            source_thread_id: row.get(2)?,
            source_thread_title: row.get(3)?,
            source_artifact_version: row.get(4)?,
            source_cfy_id: row.get(5)?,
            branch: row.get(6)?,
            question: row.get(7)?,
            reason: row.get(8)?,
            status: row.get(9)?,
            launched_thread_id: row.get(10)?,
            edited_question: row.get(11)?,
        })
    })?;
    rows.collect()
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_learning_suggestions(
    db: State<DbHandle>,
    project_id: String,
) -> Result<Vec<LearningSuggestion>, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    list(&conn, &project_id).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn dismiss_learning_suggestion(db: State<DbHandle>, id: String) -> Result<bool, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    dismiss(&conn, &id).map_err(|error| error.to_string())
}

fn dismiss(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.execute(
        "UPDATE learning_suggestions SET status = 'dismissed'
         WHERE id = ?1 AND status = 'active'",
        [id],
    )
    .map(|changed| changed > 0)
}

#[tauri::command(rename_all = "snake_case")]
pub fn record_learning_trail(
    db: State<DbHandle>,
    suggestion_id: String,
    launched_thread_id: String,
    edited_question: String,
) -> Result<(), String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    record(&conn, &suggestion_id, &launched_thread_id, &edited_question)
}

fn record(
    conn: &Connection,
    suggestion_id: &str,
    launched_thread_id: &str,
    edited_question: &str,
) -> Result<(), String> {
    let question = edited_question.trim();
    if question.is_empty() {
        return Err("edited question must not be empty".to_owned());
    }
    let same_project: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM learning_suggestions s
             JOIN threads t ON t.id = ?2 AND t.project_id = s.project_id
             WHERE s.id = ?1 AND s.status = 'active'",
            rusqlite::params![suggestion_id, launched_thread_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if same_project.is_none() {
        return Err(
            "suggestion or destination thread was not found in the same project".to_owned(),
        );
    }
    conn.execute(
        "UPDATE learning_suggestions
         SET status = 'launched', launched_thread_id = ?2, edited_question = ?3
         WHERE id = ?1 AND status = 'active'",
        rusqlite::params![suggestion_id, launched_thread_id, question],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_learning_trail(
    db: State<DbHandle>,
    thread_id: String,
) -> Result<Option<LearningTrail>, String> {
    let conn = db.lock().map_err(|error| error.to_string())?;
    trail(&conn, &thread_id).map_err(|error| error.to_string())
}

fn trail(conn: &Connection, thread_id: &str) -> rusqlite::Result<Option<LearningTrail>> {
    conn.query_row(
        "SELECT s.id, s.source_thread_id, t.title, s.source_artifact_version,
                s.source_cfy_id, s.branch, COALESCE(s.edited_question, s.question), s.reason
         FROM learning_suggestions s
         JOIN threads t ON t.id = s.source_thread_id
         WHERE s.launched_thread_id = ?1 AND s.status = 'launched'",
        [thread_id],
        |row| {
            Ok(LearningTrail {
                suggestion_id: row.get(0)?,
                source_thread_id: row.get(1)?,
                source_thread_title: row.get(2)?,
                source_artifact_version: row.get(3)?,
                source_cfy_id: row.get(4)?,
                branch: row.get(5)?,
                question: row.get(6)?,
                reason: row.get(7)?,
            })
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bounded_semantic_branches_and_skips_invalid_rows() {
        let rows = extract(
            r#"<button data-cfy-id="next-example" data-cfy-next-question="Show an example?" data-cfy-branch="example" data-cfy-reason="Applies the mechanism.">Example</button>
               <button data-cfy-id="next-bad" data-cfy-next-question="Decorate it" data-cfy-branch="generic">Bad</button>"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].branch, "example");
        assert_eq!(rows[0].source_cfy_id, "next-example");
    }

    #[test]
    fn persists_edits_dismissal_and_backtrackable_launches() {
        let path =
            std::env::temp_dir().join(format!("conceptify-learning-{}.db", uuid::Uuid::new_v4()));
        let handle = crate::db::init_at(&path).unwrap();
        let conn = handle.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, root_path) VALUES ('p1', 'P', '/tmp/p')",
            [],
        )
        .unwrap();
        for (id, title) in [("source", "Source answer"), ("next", "Edited branch")] {
            conn.execute(
                "INSERT INTO threads (id, project_id, title, slug, initial_question, status)
                 VALUES (?1, 'p1', ?2, ?1, '?', 'ready')",
                rusqlite::params![id, title],
            )
            .unwrap();
        }
        replace_for_artifact(
            &conn,
            "p1",
            "source",
            1,
            r#"<ul><li data-cfy-id="next-example" data-cfy-next-question="Show an example?" data-cfy-branch="example" data-cfy-reason="Applies the model.">Example</li><li data-cfy-id="next-tradeoff" data-cfy-next-question="What trade-off?" data-cfy-branch="tradeoff">Trade-off</li></ul>"#,
        )
        .unwrap();
        let suggestions = list(&conn, "p1").unwrap();
        assert_eq!(suggestions.len(), 2);
        let example = suggestions
            .iter()
            .find(|item| item.branch == "example")
            .unwrap();
        record(
            &conn,
            &example.id,
            "next",
            "Show a concrete parser example?",
        )
        .unwrap();
        let restored = trail(&conn, "next").unwrap().unwrap();
        assert_eq!(restored.source_thread_id, "source");
        assert_eq!(restored.question, "Show a concrete parser example?");
        let tradeoff = suggestions
            .iter()
            .find(|item| item.branch == "tradeoff")
            .unwrap();
        assert!(dismiss(&conn, &tradeoff.id).unwrap());
        assert!(list(&conn, "p1").unwrap().is_empty());
        drop(conn);
        drop(handle);
        let _ = std::fs::remove_file(path);
    }
}
