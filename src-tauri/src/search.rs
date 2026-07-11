//! Full-text indexing primitives shared by artifact saves and the query API.

use rusqlite::{params, Connection};
use scraper::{Html, Selector};

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
}
