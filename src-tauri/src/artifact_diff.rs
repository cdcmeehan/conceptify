//! Deterministic artifact-version diffing at the `data-cfy-id` anchoring unit.
//!
//! Normalization compares visible descendant text only, collapses every run of
//! Unicode whitespace to one ASCII space, and trims it. Attribute order,
//! formatting indentation, comments, and serialization differences therefore
//! cannot create changes. This deliberately also treats whitespace-only code
//! edits as unchanged; the fallback is about semantic text, not HTML bytes.
//!
//! Visible text outside all `data-cfy-id` blocks is compared as one synthetic
//! document change (`cfy_id: null`). Duplicate ids degrade to first occurrence
//! with a warning. HTML5 parsing is error-recovering, so malformed hand edits
//! return a deterministic result rather than panicking.

use std::collections::{HashMap, HashSet};
use std::fs;

use ego_tree::NodeRef;
use rusqlite::{Connection, OptionalExtension};
use scraper::{Html, Node};
use thiserror::Error;

use conceptify_types::{
    ArtifactBlockDiff, ArtifactDiffKind, ArtifactVersionDiffResponse, TextDiffHunk, TextDiffKind,
};

#[derive(Debug, Error)]
pub enum DiffError {
    #[error("thread {0} not found")]
    ThreadNotFound(String),
    #[error("artifact version {version} not found for thread {thread_id}")]
    VersionNotFound { thread_id: String, version: i64 },
    #[error("could not read artifact version {version}: {source}")]
    Read {
        version: i64,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Clone)]
struct Block {
    id: String,
    text: String,
}

#[derive(Default)]
struct ParsedArtifact {
    blocks: Vec<Block>,
    idless_text: String,
    warnings: Vec<String>,
}

pub fn diff_versions(
    conn: &Connection,
    thread_id: &str,
    from_version: i64,
    to_version: i64,
) -> Result<ArtifactVersionDiffResponse, DiffError> {
    let exists = conn
        .query_row("SELECT 1 FROM threads WHERE id = ?1", [thread_id], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    if !exists {
        return Err(DiffError::ThreadNotFound(thread_id.to_owned()));
    }
    let old = load(conn, thread_id, from_version)?;
    let new = load(conn, thread_id, to_version)?;
    Ok(diff_html(thread_id, from_version, to_version, &old, &new))
}

fn load(conn: &Connection, thread_id: &str, version: i64) -> Result<String, DiffError> {
    let path: Option<String> = conn
        .query_row(
            "SELECT file_path FROM artifacts WHERE thread_id = ?1 AND version = ?2",
            rusqlite::params![thread_id, version],
            |r| r.get(0),
        )
        .optional()?;
    let path = path.ok_or_else(|| DiffError::VersionNotFound {
        thread_id: thread_id.to_owned(),
        version,
    })?;
    fs::read_to_string(path).map_err(|source| DiffError::Read { version, source })
}

pub fn diff_html(
    thread_id: &str,
    from_version: i64,
    to_version: i64,
    old_html: &str,
    new_html: &str,
) -> ArtifactVersionDiffResponse {
    let old = parse(old_html);
    let new = parse(new_html);
    let old_by_id: HashMap<&str, usize> = old
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.as_str(), i))
        .collect();
    let new_by_id: HashMap<&str, usize> = new
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.as_str(), i))
        .collect();

    let old_common: Vec<&str> = old
        .blocks
        .iter()
        .filter(|b| new_by_id.contains_key(b.id.as_str()))
        .map(|b| b.id.as_str())
        .collect();
    let new_common: Vec<&str> = new
        .blocks
        .iter()
        .filter(|b| old_by_id.contains_key(b.id.as_str()))
        .map(|b| b.id.as_str())
        .collect();
    let stable: HashSet<&str> = lcs_values(&old_common, &new_common).into_iter().collect();

    let mut changes = Vec::new();
    let mut unchanged_count = 0;
    for (new_index, block) in new.blocks.iter().enumerate() {
        match old_by_id.get(block.id.as_str()).copied() {
            None => changes.push(block_change(
                Some(&block.id),
                ArtifactDiffKind::Added,
                false,
                None,
                Some(new_index),
                None,
                Some(&block.text),
                &new.blocks,
            )),
            Some(old_index) => {
                let old_block = &old.blocks[old_index];
                let moved = !stable.contains(block.id.as_str());
                let modified = old_block.text != block.text;
                if modified || moved {
                    changes.push(block_change(
                        Some(&block.id),
                        if modified {
                            ArtifactDiffKind::Modified
                        } else {
                            ArtifactDiffKind::Unchanged
                        },
                        moved,
                        Some(old_index),
                        Some(new_index),
                        Some(&old_block.text),
                        Some(&block.text),
                        &new.blocks,
                    ));
                } else {
                    unchanged_count += 1;
                }
            }
        }
    }
    for (old_index, block) in old.blocks.iter().enumerate() {
        if !new_by_id.contains_key(block.id.as_str()) {
            changes.push(block_change(
                Some(&block.id),
                ArtifactDiffKind::Removed,
                false,
                Some(old_index),
                None,
                Some(&block.text),
                None,
                &new.blocks,
            ));
        }
    }

    let mut warnings = old.warnings;
    warnings.extend(new.warnings);
    if old.idless_text != new.idless_text {
        warnings.push("visible text outside data-cfy-id blocks changed".to_owned());
        changes.push(ArtifactBlockDiff {
            cfy_id: None,
            kind: ArtifactDiffKind::Modified,
            moved: false,
            old_index: None,
            new_index: None,
            previous_cfy_id: None,
            next_cfy_id: None,
            old_text: Some(old.idless_text.clone()),
            new_text: Some(new.idless_text.clone()),
            hunks: text_hunks(&old.idless_text, &new.idless_text),
        });
    }

    ArtifactVersionDiffResponse {
        thread_id: thread_id.to_owned(),
        from_version,
        to_version,
        changes,
        unchanged_count,
        degraded: !warnings.is_empty(),
        warnings,
    }
}

fn block_change(
    id: Option<&str>,
    kind: ArtifactDiffKind,
    moved: bool,
    old_index: Option<usize>,
    new_index: Option<usize>,
    old_text: Option<&str>,
    new_text: Option<&str>,
    new_blocks: &[Block],
) -> ArtifactBlockDiff {
    let neighbor_index = new_index.unwrap_or_else(|| old_index.unwrap_or(0).min(new_blocks.len()));
    ArtifactBlockDiff {
        cfy_id: id.map(str::to_owned),
        kind,
        moved,
        old_index,
        new_index,
        previous_cfy_id: neighbor_index
            .checked_sub(1)
            .and_then(|i| new_blocks.get(i))
            .map(|b| b.id.clone()),
        next_cfy_id: new_blocks
            .get(neighbor_index + usize::from(new_index.is_some()))
            .map(|b| b.id.clone()),
        old_text: old_text.map(str::to_owned),
        new_text: new_text.map(str::to_owned),
        hunks: match (old_text, new_text) {
            (Some(a), Some(b)) if a != b => text_hunks(a, b),
            (Some(a), None) => vec![TextDiffHunk {
                kind: TextDiffKind::Removed,
                text: a.to_owned(),
            }],
            (None, Some(b)) => vec![TextDiffHunk {
                kind: TextDiffKind::Added,
                text: b.to_owned(),
            }],
            _ => Vec::new(),
        },
    }
}

fn parse(html: &str) -> ParsedArtifact {
    let doc = Html::parse_document(html);
    let Some(body) = doc
        .tree
        .root()
        .descendants()
        .find(|n| matches!(n.value(), Node::Element(el) if el.name() == "body"))
    else {
        return ParsedArtifact::default();
    };
    let mut parsed = ParsedArtifact::default();
    let mut seen = HashSet::new();
    let mut idless = String::new();
    for node in body.descendants() {
        match node.value() {
            Node::Element(el) => {
                if let Some(id) = el.attr("data-cfy-id") {
                    if seen.insert(id.to_owned()) {
                        parsed.blocks.push(Block {
                            id: id.to_owned(),
                            text: normalized_visible_text(node),
                        });
                    } else {
                        parsed.warnings.push(format!(
                            "duplicate data-cfy-id '{id}' used first occurrence"
                        ));
                    }
                }
            }
            Node::Text(text) if !excluded(node) && !inside_cfy_block(node) => {
                idless.push_str(text);
                idless.push(' ');
            }
            _ => {}
        }
    }
    parsed.idless_text = normalize(&idless);
    parsed
}

fn normalized_visible_text(node: NodeRef<'_, Node>) -> String {
    let mut text = String::new();
    for child in node.descendants() {
        if let Node::Text(value) = child.value() {
            if !excluded(child) {
                text.push_str(value);
                text.push(' ');
            }
        }
    }
    normalize(&text)
}

fn excluded(node: NodeRef<'_, Node>) -> bool {
    node.ancestors().any(|ancestor| {
        matches!(ancestor.value(), Node::Element(el) if matches!(el.name(), "script" | "style" | "noscript" | "template"))
    })
}

fn inside_cfy_block(node: NodeRef<'_, Node>) -> bool {
    node.ancestors().any(|ancestor| {
        matches!(ancestor.value(), Node::Element(el) if el.attr("data-cfy-id").is_some())
    })
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_hunks(old: &str, new: &str) -> Vec<TextDiffHunk> {
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    if a.len().saturating_mul(b.len()) > 1_000_000 {
        return vec![
            TextDiffHunk {
                kind: TextDiffKind::Removed,
                text: old.to_owned(),
            },
            TextDiffHunk {
                kind: TextDiffKind::Added,
                text: new.to_owned(),
            },
        ];
    }
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut raw: Vec<TextDiffHunk> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let (kind, token) = if i < a.len() && j < b.len() && a[i] == b[j] {
            let token = a[i];
            i += 1;
            j += 1;
            (TextDiffKind::Equal, token)
        } else if j < b.len() && (i == a.len() || table[i][j + 1] >= table[i + 1][j]) {
            let token = b[j];
            j += 1;
            (TextDiffKind::Added, token)
        } else {
            let token = a[i];
            i += 1;
            (TextDiffKind::Removed, token)
        };
        if let Some(last) = raw.last_mut() {
            if last.kind == kind {
                last.text.push(' ');
                last.text.push_str(token);
                continue;
            }
        }
        raw.push(TextDiffHunk {
            kind,
            text: token.to_owned(),
        });
    }
    raw
}

fn lcs_values<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
    let mut table = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(a[i]);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(body: &str) -> String {
        format!("<!doctype html><html><body>{body}</body></html>")
    }

    #[test]
    fn identical_and_serialization_only_changes_are_empty() {
        let old = html("<section class='x' data-cfy-id='a'> Hello   world </section>");
        let new =
            html("<section data-other='ignored' data-cfy-id='a' class='x'>Hello world</section>");
        let diff = diff_html("t", 1, 2, &old, &new);
        assert!(diff.changes.is_empty(), "{:#?}", diff.changes);
        assert_eq!(diff.unchanged_count, 1);
        assert!(!diff.degraded);
    }

    #[test]
    fn text_edit_is_modified_with_word_hunks() {
        let old = html("<p data-cfy-id='a'>The queue is fast</p>");
        let new = html("<p data-cfy-id='a'>The durable queue is fair</p>");
        let diff = diff_html("t", 1, 2, &old, &new);
        assert_eq!(diff.changes.len(), 1);
        let change = &diff.changes[0];
        assert_eq!(change.kind, ArtifactDiffKind::Modified);
        assert_eq!(
            change.hunks,
            vec![
                TextDiffHunk {
                    kind: TextDiffKind::Equal,
                    text: "The".into()
                },
                TextDiffHunk {
                    kind: TextDiffKind::Added,
                    text: "durable".into()
                },
                TextDiffHunk {
                    kind: TextDiffKind::Equal,
                    text: "queue is".into()
                },
                TextDiffHunk {
                    kind: TextDiffKind::Added,
                    text: "fair".into()
                },
                TextDiffHunk {
                    kind: TextDiffKind::Removed,
                    text: "fast".into()
                },
            ]
        );
    }

    #[test]
    fn insertion_deletion_and_reorder_are_distinct() {
        let old =
            html("<p data-cfy-id='a'>A</p><p data-cfy-id='b'>B</p><p data-cfy-id='gone'>G</p>");
        let new =
            html("<p data-cfy-id='b'>B</p><p data-cfy-id='a'>A</p><p data-cfy-id='new'>N</p>");
        let diff = diff_html("t", 1, 2, &old, &new);
        assert!(diff
            .changes
            .iter()
            .any(|c| c.cfy_id.as_deref() == Some("new") && c.kind == ArtifactDiffKind::Added));
        assert!(diff
            .changes
            .iter()
            .any(|c| c.cfy_id.as_deref() == Some("gone") && c.kind == ArtifactDiffKind::Removed));
        let moved: Vec<_> = diff.changes.iter().filter(|c| c.moved).collect();
        assert_eq!(moved.len(), 1, "one member of the swap is the minimal move");
        assert_eq!(moved[0].kind, ArtifactDiffKind::Unchanged);
    }

    #[test]
    fn idless_change_degrades_to_document_fallback_and_malformed_html_is_safe() {
        let old = html("loose before<div data-cfy-id='a'>Block");
        let new = html("loose after<div data-cfy-id='a'>Block</div>");
        let diff = diff_html("t", 1, 2, &old, &new);
        assert!(diff.degraded);
        let fallback = diff.changes.iter().find(|c| c.cfy_id.is_none()).unwrap();
        assert_eq!(fallback.kind, ArtifactDiffKind::Modified);
        assert!(fallback
            .hunks
            .iter()
            .any(|h| h.kind == TextDiffKind::Removed));
        assert!(fallback.hunks.iter().any(|h| h.kind == TextDiffKind::Added));
    }

    #[test]
    fn hundred_block_artifact_is_well_below_one_second() {
        let old_body: String = (0..150)
            .map(|i| format!("<p data-cfy-id='b{i}'>Block {i} stable text</p>"))
            .collect();
        let new_body = old_body.replace("Block 73 stable", "Block 73 changed");
        let started = std::time::Instant::now();
        let diff = diff_html("t", 1, 2, &html(&old_body), &html(&new_body));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].cfy_id.as_deref(), Some("b73"));
    }
}
