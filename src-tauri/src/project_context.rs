//! Lightweight, local-only project orientation. This is deliberately not an
//! index: it counts a bounded set of files/languages and records exclusions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::DbHandle;

const MAX_FILES: usize = 5_000;
const EXCLUDED: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", ".venv", "vendor"];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LanguageCount {
    pub name: String,
    pub files: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProjectContextSummary {
    pub status: String,
    pub repository: String,
    pub languages: Vec<LanguageCount>,
    pub included_files: usize,
    pub excluded_paths: Vec<String>,
    pub fingerprint: String,
    pub scanned_at: String,
    pub warning: Option<String>,
    pub unchanged: bool,
}

fn key(project_id: &str) -> String {
    format!("project_context:{project_id}")
}

pub fn stored(conn: &rusqlite::Connection, project_id: &str) -> Option<ProjectContextSummary> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key(project_id)], |r| r.get::<_, String>(0))
        .optional().ok().flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
}

fn modified(path: &Path) -> u64 {
    path.metadata().and_then(|m| m.modified()).ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs()).unwrap_or(0)
}

fn fingerprint(root: &Path) -> String {
    let top_level = std::fs::read_dir(root).map(|entries| entries.filter_map(Result::ok).count()).unwrap_or(0);
    format!("{}:{}:{}", modified(root), modified(&root.join(".git/index")), top_level)
}

fn language(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "rs" => "Rust", "ts" | "tsx" => "TypeScript", "js" | "jsx" | "mjs" => "JavaScript",
        "py" => "Python", "go" => "Go", "java" => "Java", "kt" | "kts" => "Kotlin",
        "swift" => "Swift", "rb" => "Ruby", "php" => "PHP", "cs" => "C#",
        "c" | "h" => "C", "cc" | "cpp" | "cxx" | "hpp" => "C++", "html" => "HTML",
        "css" | "scss" | "sass" => "CSS", "md" | "mdx" => "Markdown", "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell", "json" | "yaml" | "yml" | "toml" => "Configuration",
        _ => return None,
    })
}

fn scan(root: &Path, previous: Option<&ProjectContextSummary>) -> Result<ProjectContextSummary, String> {
    let fingerprint = fingerprint(root);
    if let Some(previous) = previous {
        if previous.fingerprint == fingerprint {
            let mut same = previous.clone();
            same.unchanged = true;
            return Ok(same);
        }
    }
    let mut stack = vec![root.to_path_buf()];
    let mut counts = BTreeMap::<String, usize>::new();
    let mut included_files = 0;
    let mut excluded = Vec::new();
    let mut limited = false;
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) if directory == root => return Err("This folder can’t be read. Choose another folder.".to_owned()),
            Err(_) => continue,
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if EXCLUDED.contains(&name.as_str()) {
                    if !excluded.contains(&name) { excluded.push(name); }
                } else {
                    stack.push(path);
                }
                continue;
            }
            included_files += 1;
            if let Some(name) = path.extension().and_then(|value| value.to_str()).and_then(language) {
                *counts.entry(name.to_owned()).or_default() += 1;
            }
            if included_files >= MAX_FILES { limited = true; break; }
        }
        if limited { break; }
    }
    let mut languages: Vec<_> = counts.into_iter().map(|(name, files)| LanguageCount { name, files }).collect();
    languages.sort_by(|a, b| b.files.cmp(&a.files).then_with(|| a.name.cmp(&b.name)));
    languages.truncate(6);
    excluded.sort();
    Ok(ProjectContextSummary {
        status: if limited { "limited" } else { "ready" }.to_owned(),
        repository: if root.join(".git").exists() { "Git repository" } else { "Folder" }.to_owned(),
        languages,
        included_files,
        excluded_paths: excluded,
        fingerprint,
        scanned_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        warning: limited.then(|| format!("Orientation stopped after {MAX_FILES} files. You can still ask questions; agents read relevant files directly.")),
        unchanged: false,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn scan_project_context(db: State<'_, DbHandle>, project_id: String) -> Result<ProjectContextSummary, String> {
    let (root, previous) = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        let root: String = conn.query_row("SELECT root_path FROM projects WHERE id = ?1", [&project_id], |r| r.get(0)).map_err(|_| format!("project not found: {project_id}"))?;
        (PathBuf::from(root), stored(&conn, &project_id))
    };
    let scan_fingerprint = fingerprint(&root);
    let summary = match tokio::task::spawn_blocking(move || scan(&root, previous.as_ref())).await.map_err(|e| e.to_string())? {
        Ok(summary) => summary,
        Err(message) => ProjectContextSummary {
            status: "error".to_owned(), repository: "Folder".to_owned(), languages: Vec::new(),
            included_files: 0, excluded_paths: EXCLUDED.iter().map(|value| (*value).to_owned()).collect(),
            fingerprint: scan_fingerprint, scanned_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            warning: Some(message), unchanged: false,
        },
    };
    let json = serde_json::to_string(&summary).map_err(|e| e.to_string())?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", rusqlite::params![key(&project_id), json]).map_err(|e| e.to_string())?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_scan_detects_languages_exclusions_and_unchanged_fingerprint() {
        let root = std::env::temp_dir().join(format!("conceptify-context-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/app.ts"), "export {};").unwrap();
        let first = scan(&root, None).unwrap();
        assert_eq!(first.repository, "Git repository");
        assert_eq!(first.included_files, 2);
        assert!(first.excluded_paths.contains(&"node_modules".to_owned()));
        assert!(first.languages.iter().any(|item| item.name == "Rust"));
        assert!(scan(&root, Some(&first)).unwrap().unchanged);
        std::fs::remove_dir_all(root).unwrap();
    }
}
