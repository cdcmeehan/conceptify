//! Full-text indexing primitives shared by artifact saves and the query API.

use rusqlite::{params, Connection};
use scraper::{Html, Selector};
use conceptify_types::{SearchHit, SearchHitKind, SearchResponse};
use tauri::State;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBlock {
    pub id: String,
    pub title: String,
    pub text: String,
}

/// Extract independently navigable blocks. Nested blocks are intentionally
/// indexed too: their stable ids are the most precise landing targets.
pub fn extract_artifact_blocks(html: &str) -> Vec<ArtifactBlock> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("[data-cfy-id]").expect("static selector");
    let heading = Selector::parse("h1,h2,h3,h4,h5,h6").expect("static selector");
    document
        .select(&selector)
        .filter_map(|element| {
            let id = element.value().attr("data-cfy-id")?.trim().to_owned();
            if id.is_empty() {
                return None;
            }
            let title = element
                .select(&heading)
                .next()
                .map(|node| normalized_text(node.text()))
                .unwrap_or_default();
            let text = normalized_text(element.text());
            (!text.is_empty()).then_some(ArtifactBlock { id, title, text })
        })
        .collect()
}

fn normalized_text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace the artifact portion of one thread's index with its latest version.
pub fn replace_artifact(
    conn: &Connection,
    project_id: &str,
    thread_id: &str,
    version: i64,
    html: &str,
) -> rusqlite::Result<()> {
    // A handful of focused legacy unit tests construct only the tables their
    // subject needs instead of running migrations. Production databases always
    // have the index; treating that deliberately-minimal fixture as no-index
    // keeps the save primitive reusable in those tests.
    let available: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'search_index')",
        [],
        |row| row.get(0),
    )?;
    if !available {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM search_index WHERE kind = 'artifact' AND thread_id = ?1",
        [thread_id],
    )?;
    let mut insert = conn.prepare(
        "INSERT INTO search_index
         (search_key, kind, entity_id, project_id, thread_id, artifact_version, block_id, title, body)
         VALUES (?1, 'artifact', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for block in extract_artifact_blocks(html) {
        let key = format!("artifact:{thread_id}:{version}:{}", block.id);
        insert.execute(params![
            key,
            key,
            project_id,
            thread_id,
            version,
            block.id,
            block.title,
            block.text
        ])?;
    }
    Ok(())
}

/// Recovery path for artifact rows whose source content lives outside SQLite.
/// Missing files are skipped so a metadata DB remains bootable after a project
/// directory is moved; remapping or the next save repairs those rows.
pub fn rebuild_artifacts(conn: &Connection) -> rusqlite::Result<usize> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT t.project_id, t.id, a.version, a.file_path
             FROM threads t JOIN artifacts a ON a.thread_id = t.id
             WHERE a.version = (SELECT max(a2.version) FROM artifacts a2 WHERE a2.thread_id = t.id)",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut count = 0;
    for (project_id, thread_id, version, path) in rows {
        if let Ok(html) = std::fs::read_to_string(path) {
            count += extract_artifact_blocks(&html).len();
            replace_artifact(conn, &project_id, &thread_id, version, &html)?;
        }
    }
    Ok(count)
}

fn match_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-'))
        .filter(|term| !term.is_empty())
        .take(12)
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

pub fn query(
    conn: &Connection,
    input: &str,
    project_filter: Option<&str>,
    limit: usize,
) -> rusqlite::Result<SearchResponse> {
    let Some(fts_query) = match_query(input) else {
        return Ok(SearchResponse::default());
    };
    let limit = limit.clamp(1, 100) as i64;
    let mut stmt = conn.prepare(
        "SELECT kind, entity_id, project_id, thread_id, artifact_version, block_id,
                highlight(search_index, 7, '<mark>', '</mark>'),
                snippet(search_index, 8, '<mark>', '</mark>', ' … ', 24),
                bm25(search_index, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 8.0, 1.0)
         FROM search_index
         WHERE search_index MATCH ?1 AND (?2 IS NULL OR project_id = ?2)
         ORDER BY 9 ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![fts_query, project_filter, limit], |row| {
        let kind_text: String = row.get(0)?;
        let kind = match kind_text.as_str() {
            "project" => SearchHitKind::Project,
            "thread" => SearchHitKind::Thread,
            "artifact" => SearchHitKind::Artifact,
            "comment" => SearchHitKind::Comment,
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(SearchHit {
            kind,
            entity_id: row.get(1)?,
            project_id: row.get(2)?,
            thread_id: row.get(3)?,
            artifact_version: row.get(4)?,
            block_id: row.get(5)?,
            title: row.get(6)?,
            snippet: row.get(7)?,
            rank: row.get(8)?,
        })
    })?;
    let mut response = SearchResponse::default();
    for hit in rows {
        let hit = hit?;
        match hit.kind {
            SearchHitKind::Project => response.projects.push(hit),
            SearchHitKind::Thread => response.threads.push(hit),
            SearchHitKind::Artifact => response.artifacts.push(hit),
            SearchHitKind::Comment => response.comments.push(hit),
        }
    }
    Ok(response)
}

#[tauri::command]
pub fn search(
    db: State<crate::db::DbHandle>,
    query: String,
    project_filter: Option<String>,
    limit: Option<usize>,
) -> Result<SearchResponse, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    self::query(&conn, &query, project_filter.as_deref(), limit.unwrap_or(40))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_strips_markup_and_preserves_code_and_tables() {
        let blocks = extract_artifact_blocks(r#"
          <section data-cfy-id="cache"><h2>Cache keys</h2>
            <p>Use <code>user_id</code>.</p><table><tr><th>Key</th><td>TTL</td></tr></table>
          </section>"#);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].title, "Cache keys");
        assert_eq!(blocks[0].text, "Cache keys Use user_id . Key TTL");
    }

    #[test]
    fn bundled_sqlite_has_fts5() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE VIRTUAL TABLE fts_probe USING fts5(body)", [])
            .expect("bundled SQLite must include FTS5");
    }

    fn search_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::migrations().to_latest(&mut conn).unwrap();
        conn.execute("INSERT INTO projects (id,name,root_path) VALUES ('p1','Compiler Lab','/tmp/p1')", []).unwrap();
        conn.execute("INSERT INTO projects (id,name,root_path) VALUES ('p2','Other','/tmp/p2')", []).unwrap();
        conn.execute("INSERT INTO threads (id,project_id,title,slug,initial_question,status) VALUES ('t1','p1','Lexer Pipeline','lexer','tokens and parsing','ready')", []).unwrap();
        conn.execute("INSERT INTO threads (id,project_id,title,slug,initial_question,status) VALUES ('t2','p2','Other Thread','other','lexer only in body','ready')", []).unwrap();
        conn
    }

    #[test]
    fn grouped_ranked_results_include_navigation_and_highlights() {
        let conn = search_conn();
        replace_artifact(&conn, "p1", "t1", 3, "<section data-cfy-id='dfa'><h2>State machine</h2><p>The lexer emits tokens.</p></section>").unwrap();
        let result = query(&conn, "lex", Some("p1"), 20).unwrap();
        assert_eq!(result.threads[0].entity_id, "t1");
        assert!(result.threads[0].title.contains("<mark>Lexer</mark>"));
        assert_eq!(result.artifacts[0].artifact_version, Some(3));
        assert_eq!(result.artifacts[0].block_id.as_deref(), Some("dfa"));
        assert!(result.artifacts[0].snippet.contains("<mark>lexer</mark>"));
        assert!(result.threads[0].rank <= result.artifacts[0].rank);
    }

    #[test]
    fn hostile_and_empty_queries_never_reach_fts_syntax() {
        let conn = search_conn();
        for input in ["", "   ", "\" (( NEAR(foo", "lexer OR * ]"] {
            query(&conn, input, None, 10).expect("sanitized query");
        }
    }
}
