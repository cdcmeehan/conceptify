//! Anchor re-attachment across artifact versions (PRD FR-4.4, §7.4; bead
//! `conceptify-94m.7`).
//!
//! When a new artifact version is saved, every open/answered comment anchored
//! to an earlier version is re-attached against the NEW document:
//!
//! - **Primary strategy**: the anchor's `data-cfy-id` still resolves and (for
//!   text anchors) the stored offsets still select text equal to the stored
//!   `quote.exact`. Nothing is rewritten.
//! - **Fallback**: a W3C-style text-quote search (`exact`, disambiguated by
//!   `prefix`/`suffix`) — scoped to the original `cfy_id` element first, then
//!   document-wide. On success the anchor's *primary* fields (`cfy_id`,
//!   `start`, `end`) are rewritten to the new location; the `quote` is never
//!   rewritten (it is the user's captured selection and remains the durable
//!   fallback).
//! - **Failure** → the comment is flagged `anchor_state = 'moved'` ("reference
//!   moved") and its `artifact_version` stays at the last version where the
//!   anchor resolved — NEVER silently dropped. It is retried on every later
//!   save, so it can heal if the content returns.
//!
//! This is what makes the M5 apply-to-artifact loop safe: applying one
//! clarification can't invisibly orphan the other comments.
//!
//! **Measurement contract** (must match `src-tauri/assets/bridge.js` — see
//! docs/api.md "Bridge protocol → Conventions"): text-anchor offsets are
//! UTF-16 code-unit indices into the element's *visible text* — the
//! concatenation of its `Text` node data in document order, excluding text
//! inside `script`/`style`/`noscript`/`template` subtrees, with no whitespace
//! normalization. Document-wide text (for `prefix`/`suffix` context) is the
//! visible text of `<body>`. Element-anchor quotes are compared against the
//! element's whitespace-collapsed + trimmed *full* text (`textContent`
//! semantics — script/style included), matching the bridge's capture.
//!
//! The module is deliberately split: [`DocumentIndex`] + [`Outcome`] are a
//! pure, DB-free core (unit-testable against raw HTML), and
//! [`reattach_thread_comments`] is the thin orchestration the save pipeline
//! (`artifacts::save_artifact`) calls inside its transaction.

use ego_tree::iter::Edge;
use rusqlite::Connection;
use scraper::{Html, Node};
use serde_json::{json, Value};

use crate::comments::{self, AnchorState, Comment};

/// Cap on quote-search candidate positions (mirrors the bridge's guard against
/// degenerate documents).
const MAX_QUOTE_CANDIDATES: usize = 200;

/// Subtrees whose text is excluded from visible-text measurement (the bridge's
/// `isSkipped` set).
fn is_excluded_tag(name: &str) -> bool {
    matches!(name, "script" | "style" | "noscript" | "template")
}

/// The verdict for one anchor against the new document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The anchor resolves in the new document. `rewritten` carries the
    /// updated anchor JSON when the primary fields had to be re-pointed
    /// (offsets shifted, `cfy_id` changed, or primary dropped to quote-only);
    /// `None` means the stored anchor is intact as-is.
    Anchored { rewritten: Option<Value> },
    /// The anchor cannot be re-located: flag "reference moved".
    Moved,
}

/// A `data-cfy-id`-bearing element in the new document.
struct CfyElement {
    id: String,
    /// Span of the element's visible text within [`DocumentIndex::text`]
    /// (UTF-16 code units). An element's text nodes are contiguous in the
    /// document-order concatenation, so its visible text is exactly
    /// `text[start..end]`.
    start: usize,
    end: usize,
    /// Nesting depth among cfy elements (for deepest-containment mapping).
    depth: usize,
    /// Full descendant text (`textContent` semantics: script/style included),
    /// accumulated during the walk; collapsed once at the end.
    full: String,
    /// Whitespace-collapsed + trimmed `full` (the element-anchor quote
    /// convention).
    collapsed: String,
}

/// A parsed-and-measured view of a new artifact version, ready to answer
/// "does this anchor still resolve?" for any stored anchor.
pub struct DocumentIndex {
    /// Visible text of `<body>` in UTF-16 code units (the bridge's
    /// document-wide measurement base).
    text: Vec<u16>,
    /// Every `data-cfy-id`-bearing element under `<body>`, in document order.
    elements: Vec<CfyElement>,
}

impl DocumentIndex {
    /// Parse and measure an artifact document. Never fails: HTML5 parsing is
    /// error-recovering, and a document without a `<body>` simply yields an
    /// empty index (every anchor will come out `Moved`, which is the correct
    /// verdict for a document with no content).
    pub fn parse(html: &str) -> DocumentIndex {
        let doc = Html::parse_document(html);
        let mut index = DocumentIndex {
            text: Vec::new(),
            elements: Vec::new(),
        };

        let Some(body) = doc
            .tree
            .root()
            .descendants()
            .find(|n| matches!(n.value(), Node::Element(el) if el.name() == "body"))
        else {
            return index;
        };

        // Iterative open/close traversal (no recursion — a pathologically
        // nested document must not overflow the stack; same reasoning as the
        // validator's flat walk).
        let mut excluded_depth = 0usize;
        let mut open: Vec<usize> = Vec::new(); // indices into `elements`

        for edge in body.traverse() {
            match edge {
                Edge::Open(n) => match n.value() {
                    Node::Element(el) => {
                        if is_excluded_tag(el.name()) {
                            excluded_depth += 1;
                        }
                        if let Some(id) = el.attr("data-cfy-id") {
                            let i = index.elements.len();
                            index.elements.push(CfyElement {
                                id: id.to_owned(),
                                start: index.text.len(),
                                end: index.text.len(),
                                depth: open.len(),
                                full: String::new(),
                                collapsed: String::new(),
                            });
                            open.push(i);
                        }
                    }
                    Node::Text(t) => {
                        let data: &str = t;
                        if excluded_depth == 0 {
                            index.text.extend(data.encode_utf16());
                        }
                        // Full text (textContent semantics) feeds the
                        // element-anchor quote comparison; excluded subtrees
                        // still count there.
                        for &i in &open {
                            index.elements[i].full.push_str(data);
                        }
                    }
                    _ => {}
                },
                Edge::Close(n) => {
                    if let Node::Element(el) = n.value() {
                        if el.attr("data-cfy-id").is_some() {
                            if let Some(i) = open.pop() {
                                index.elements[i].end = index.text.len();
                            }
                        }
                        if is_excluded_tag(el.name()) {
                            excluded_depth -= 1;
                        }
                    }
                }
            }
        }

        for el in &mut index.elements {
            el.collapsed = collapse_ws(&el.full);
        }
        index
    }

    /// First element (document order) carrying `id` — mirrors the bridge's
    /// `querySelector` resolution when `W-ID-DUP` duplicates slip through.
    fn element_by_id(&self, id: &str) -> Option<&CfyElement> {
        self.elements.iter().find(|e| e.id == id)
    }

    /// Deepest cfy element whose visible-text span fully contains
    /// `[at, at + len)`.
    fn deepest_containing(&self, at: usize, len: usize) -> Option<&CfyElement> {
        self.elements
            .iter()
            .filter(|e| e.start <= at && at + len <= e.end)
            .max_by_key(|e| e.depth)
    }

    /// Re-attach one stored anchor against this document. The input is the
    /// verbatim stored JSON (validated at create time); a value that no longer
    /// parses as a known anchor is `Moved` (flag, never drop).
    pub fn reattach(&self, anchor: &Value) -> Outcome {
        match serde_json::from_value::<conceptify_types::Anchor>(anchor.clone()) {
            Ok(conceptify_types::Anchor::Text(t)) => self.reattach_text(anchor, &t),
            Ok(conceptify_types::Anchor::Element(e)) => self.reattach_element(anchor, &e),
            Err(_) => Outcome::Moved,
        }
    }

    fn reattach_text(&self, original: &Value, t: &conceptify_types::TextAnchor) -> Outcome {
        let exact16: Vec<u16> = t.quote.exact.encode_utf16().collect();
        if exact16.is_empty() {
            // A text anchor always carries a non-empty quote (the bridge
            // rejects whitespace-only selections); an empty one can't be
            // located.
            return Outcome::Moved;
        }

        let host = t.cfy_id.as_deref().and_then(|id| self.element_by_id(id));

        // Primary: same cfy_id + offsets still select text equal to the quote.
        if let (Some(host), Some(start), Some(end)) = (host, t.start, t.end) {
            let (start, end) = (start as usize, end as usize);
            if start <= end
                && host.start + end <= host.end
                && self.text[host.start + start..host.start + end] == exact16[..]
            {
                return Outcome::Anchored { rewritten: None };
            }
        }

        // Fallback: quote search over the document's visible text.
        let candidates = find_all(&self.text, &exact16, MAX_QUOTE_CANDIDATES);
        if candidates.is_empty() {
            return Outcome::Moved;
        }
        let prefix16: Option<Vec<u16>> = t
            .quote
            .prefix
            .as_deref()
            .map(|p| p.encode_utf16().collect());
        let suffix16: Option<Vec<u16>> = t
            .quote
            .suffix
            .as_deref()
            .map(|s| s.encode_utf16().collect());
        let score = |at: usize| {
            context_score(
                &self.text,
                at,
                exact16.len(),
                prefix16.as_deref(),
                suffix16.as_deref(),
            )
        };

        // Tier A: matches inside the original cfy_id element (the quote moved
        // *within* its section). Ambiguity at the deciding tier is `Moved` —
        // re-attachment is a persistent decision, so unlike the bridge's
        // best-effort decoration it never gambles on a tie.
        if let Some(host) = host {
            let in_host: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|&at| host.start <= at && at + exact16.len() <= host.end)
                .collect();
            if !in_host.is_empty() {
                return match unique_best(&in_host, score) {
                    Some(at) => Outcome::Anchored {
                        rewritten: Some(rewrite_text_primary(
                            original,
                            Some((&host.id, at - host.start, at - host.start + exact16.len())),
                        )),
                    },
                    None => Outcome::Moved,
                };
            }
        }

        // Tier B: document-wide (element renamed/removed, or the quote moved
        // to a different section — or the anchor was quote-only all along).
        match unique_best(&candidates, score) {
            Some(at) => {
                // A quote-only anchor (never had primary fields) that still
                // resolves needs no rewrite: primary fields are only ever
                // *repaired*, never invented — the quote is the anchor.
                if t.cfy_id.is_none() && t.start.is_none() && t.end.is_none() {
                    return Outcome::Anchored { rewritten: None };
                }
                let rewrite = self
                    .deepest_containing(at, exact16.len())
                    .map(|el| (el.id.as_str(), at - el.start, at - el.start + exact16.len()));
                Outcome::Anchored {
                    rewritten: Some(rewrite_text_primary(original, rewrite)),
                }
            }
            None => Outcome::Moved,
        }
    }

    fn reattach_element(
        &self,
        original: &Value,
        e: &conceptify_types::ElementAnchor,
    ) -> Outcome {
        // Primary: the id survives (spec §4.3 says ids are never renamed, so
        // this is the overwhelmingly common case).
        if self.element_by_id(&e.cfy_id).is_some() {
            return Outcome::Anchored { rewritten: None };
        }

        // Fallback: the id vanished — find the id-bearing element whose
        // collapsed text equals the stored quote (captured collapsed, but
        // collapse again defensively).
        let Some(target) = e
            .quote
            .as_ref()
            .map(|q| collapse_ws(&q.exact))
            .filter(|t| !t.is_empty())
        else {
            return Outcome::Moved;
        };

        let mut matches: Vec<&CfyElement> = self
            .elements
            .iter()
            .filter(|el| el.collapsed == target)
            .collect();
        // Nested id-bearing elements can collapse to identical text (e.g. a
        // figure group and its label). A strict nesting chain is not
        // ambiguous — pick the innermost; genuinely parallel matches are.
        matches.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let innermost = match matches.as_slice() {
            [] => return Outcome::Moved,
            [only] => only,
            all => {
                let chained = all
                    .windows(2)
                    .all(|w| w[0].start <= w[1].start && w[1].end <= w[0].end);
                if !chained {
                    return Outcome::Moved;
                }
                all.last().unwrap()
            }
        };

        let mut v = original.clone();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("cfy_id".to_owned(), json!(innermost.id));
        }
        Outcome::Anchored { rewritten: Some(v) }
    }
}

/// Rewrite a text anchor's primary fields, preserving everything else
/// (envelope, quote, bridge capture hints) verbatim. `Some((id, start, end))`
/// re-points the primary; `None` drops it to a quote-only anchor.
fn rewrite_text_primary(original: &Value, primary: Option<(&str, usize, usize)>) -> Value {
    let mut v = original.clone();
    if let Some(obj) = v.as_object_mut() {
        match primary {
            Some((id, start, end)) => {
                obj.insert("cfy_id".to_owned(), json!(id));
                obj.insert("start".to_owned(), json!(start));
                obj.insert("end".to_owned(), json!(end));
            }
            None => {
                obj.remove("cfy_id");
                obj.remove("start");
                obj.remove("end");
            }
        }
    }
    v
}

/// All occurrences of `needle` in `haystack` (UTF-16 units), capped.
fn find_all(haystack: &[u16], needle: &[u16], cap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return out;
    }
    for at in 0..=haystack.len() - needle.len() {
        if haystack[at..at + needle.len()] == *needle {
            out.push(at);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// W3C-style context score for a candidate match: +1 for an exact `prefix`
/// match immediately before, +1 for an exact `suffix` match immediately after.
/// Absent context contributes nothing (no constraint).
fn context_score(
    text: &[u16],
    at: usize,
    len: usize,
    prefix: Option<&[u16]>,
    suffix: Option<&[u16]>,
) -> u32 {
    let mut score = 0;
    if let Some(p) = prefix {
        if at >= p.len() && text[at - p.len()..at] == *p {
            score += 1;
        }
    }
    if let Some(s) = suffix {
        let end = at + len;
        if end + s.len() <= text.len() && text[end..end + s.len()] == *s {
            score += 1;
        }
    }
    score
}

/// The single candidate with the strictly highest score, or `None` when the
/// maximum is tied (ambiguous — the caller flags `moved` rather than guess).
fn unique_best(candidates: &[usize], score: impl Fn(usize) -> u32) -> Option<usize> {
    let mut best_at = None;
    let mut best_score = 0u32;
    let mut best_count = 0usize;
    for &at in candidates {
        let s = score(at);
        if best_at.is_none() || s > best_score {
            best_at = Some(at);
            best_score = s;
            best_count = 1;
        } else if s == best_score {
            best_count += 1;
        }
    }
    if best_count == 1 {
        best_at
    } else {
        None
    }
}

/// Whitespace-collapse + trim (the element-anchor quote convention; Rust's
/// `char::is_whitespace` stands in for the bridge's `/\s+/`).
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Orchestration: the save-pipeline hook
// ---------------------------------------------------------------------------

/// Re-attach a thread's comments against a just-saved new version. Called by
/// `artifacts::save_artifact` inside its transaction, after the new artifact
/// row exists (so the advanced `artifact_version` satisfies the composite FK)
/// — the version row and every comment mutation commit atomically.
///
/// Participation (documented in docs/api.md "Re-attachment across versions"):
/// every comment with `artifact_version < new_version` and status `open` or
/// `answered`. `applied` comments are frozen history (the apply itself
/// typically rewrote their anchored text — re-flagging them "moved" would be
/// noise, not signal). Previously-`moved` comments participate again on every
/// save, so they heal when the content returns. Null-anchor comments (direct
/// follow-ups) are version-agnostic and advance trivially.
///
/// Returns the comments whose rows actually changed (for `comment-updated`
/// events); an untouched row (e.g. already `moved`, still unresolvable) emits
/// nothing.
pub fn reattach_thread_comments(
    conn: &Connection,
    html: &str,
    thread_id: &str,
    new_version: i64,
) -> Result<Vec<Comment>, rusqlite::Error> {
    let candidates = comments::reattach_candidates(conn, thread_id, new_version)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let index = DocumentIndex::parse(html);
    let mut changed = Vec::new();

    for comment in candidates {
        let (version, anchor_rewrite, state) = match &comment.anchor {
            // Direct follow-ups are version-agnostic: follow the latest.
            None => (new_version, None, AnchorState::Anchored),
            Some(anchor) => match index.reattach(anchor) {
                Outcome::Anchored { rewritten } => {
                    (new_version, rewritten, AnchorState::Anchored)
                }
                // Failure: keep artifact_version at the last version where
                // the anchor resolved — the (version, anchor) pair stays
                // truthful (switching the viewer there still highlights it).
                Outcome::Moved => (comment.artifact_version, None, AnchorState::Moved),
            },
        };

        let row_changes = version != comment.artifact_version
            || state != comment.anchor_state
            || anchor_rewrite.is_some();
        if !row_changes {
            continue;
        }

        let updated = comments::apply_reattachment(
            conn,
            &comment.id,
            version,
            anchor_rewrite.as_ref(),
            state,
        )?;
        changed.push(updated);
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Index a minimal document whose `<body>` is exactly `body`. The unit
    /// tests model "v1 was captured, v2 arrived": the anchor literals below
    /// are what the bridge would have captured against the v1 content named
    /// in each test, and `body` is the NEW (v2) content.
    fn idx(body: &str) -> DocumentIndex {
        DocumentIndex::parse(&format!(
            "<!doctype html><html><head><title>t</title></head><body>{body}</body></html>"
        ))
    }

    fn text_anchor(cfy_id: &str, start: u32, end: u32, exact: &str) -> Value {
        json!({
            "v": 1, "type": "text", "cfy_id": cfy_id, "start": start, "end": end,
            "quote": { "exact": exact }
        })
    }

    /// Unwrap `Anchored { rewritten: Some(_) }`.
    fn rewritten(outcome: Outcome) -> Value {
        match outcome {
            Outcome::Anchored {
                rewritten: Some(v),
            } => v,
            other => panic!("expected Anchored with rewrite, got {other:?}"),
        }
    }

    // -- text anchors: primary ----------------------------------------------

    #[test]
    fn unchanged_element_offsets_hold_without_rewrite() {
        // v1 == v2: "alpha beta gamma", selection "beta" at [6, 10).
        let d = idx(r#"<p data-cfy-id="sec-a">alpha beta gamma</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Anchored { rewritten: None });
    }

    #[test]
    fn offsets_reject_when_slice_no_longer_matches_quote() {
        // Same length, different content at [6, 10) — offsets alone are not
        // trusted; the quote verification forces the fallback, which fails
        // (the quote text is gone entirely).
        let d = idx(r#"<p data-cfy-id="sec-a">alpha zeta gamma</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn script_and_style_text_is_excluded_from_offsets() {
        // Inline script/style text before the quote must not shift offsets
        // (the bridge measures the same way — that's the whole point).
        let d = idx(
            r#"<p data-cfy-id="sec-a"><script>var beta = 1;</script><style>.beta{}</style>alpha beta</p>"#,
        );
        let a = text_anchor("sec-a", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Anchored { rewritten: None });
    }

    // -- text anchors: quote fallback ----------------------------------------

    #[test]
    fn edited_element_requotes_within_same_element() {
        // v1: "alpha beta gamma" (beta at 6). v2 prepends "intro ": offsets
        // stale, quote re-found inside the same cfy element at 12.
        let d = idx(r#"<p data-cfy-id="sec-a">intro alpha beta gamma</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "sec-a");
        assert_eq!(r["start"], 12);
        assert_eq!(r["end"], 16);
        // Everything else (envelope, quote) is preserved verbatim.
        assert_eq!(r["v"], 1);
        assert_eq!(r["quote"]["exact"], "beta");
    }

    #[test]
    fn renamed_element_found_document_wide() {
        // v1 host id "sec-a" is gone; the quote lives in "sec-b" now.
        let d = idx(r#"<p data-cfy-id="sec-b">alpha beta gamma</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "sec-b");
        assert_eq!(r["start"], 6);
        assert_eq!(r["end"], 10);
    }

    #[test]
    fn quote_moved_to_a_different_section() {
        // Host still exists but no longer contains the quote; it moved to a
        // sibling section.
        let d = idx(
            r#"<p data-cfy-id="sec-a">totally new words</p><p data-cfy-id="sec-z">alpha beta gamma</p>"#,
        );
        let a = text_anchor("sec-a", 6, 10, "beta");
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "sec-z");
        assert_eq!(r["start"], 6);
        assert_eq!(r["end"], 10);
    }

    #[test]
    fn match_outside_any_cfy_element_drops_to_quote_only() {
        // The quote survives but in un-anchored prose: primary fields are
        // removed, the quote remains the sole anchor.
        let d = idx(r#"<p>alpha beta gamma</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        let r = rewritten(d.reattach(&a));
        assert!(r.get("cfy_id").is_none());
        assert!(r.get("start").is_none());
        assert!(r.get("end").is_none());
        assert_eq!(r["quote"]["exact"], "beta");
    }

    #[test]
    fn prefix_suffix_disambiguate_repeated_quote() {
        let d = idx(
            r#"<p data-cfy-id="s1">alpha beta one</p><p data-cfy-id="s2">alpha beta two</p>"#,
        );
        let a = json!({
            "v": 1, "type": "text", "cfy_id": "sec-old", "start": 6, "end": 10,
            "quote": { "exact": "beta", "suffix": " two" }
        });
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "s2");
        assert_eq!(r["start"], 6);
        assert_eq!(r["end"], 10);
    }

    #[test]
    fn genuinely_ambiguous_quote_is_moved() {
        // Two indistinguishable matches, no disambiguating context: never
        // gamble — flag "reference moved".
        let d = idx(
            r#"<p data-cfy-id="s1">alpha beta one</p><p data-cfy-id="s2">alpha beta two</p>"#,
        );
        let a = text_anchor("sec-old", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn ambiguity_within_host_element_is_moved_not_guessed() {
        // Both matches are inside the original host, tied on context.
        let d = idx(r#"<p data-cfy-id="sec-a">beta and beta</p>"#);
        let a = text_anchor("sec-a", 20, 24, "beta");
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn in_host_match_beats_identical_match_elsewhere() {
        // The quote appears once inside the original host and once outside;
        // the element-scoped tier wins without needing context.
        let d = idx(
            r#"<p data-cfy-id="sec-a">now beta here</p><p data-cfy-id="s2">also beta there</p>"#,
        );
        let a = text_anchor("sec-a", 6, 10, "beta");
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "sec-a");
        assert_eq!(r["start"], 4);
        assert_eq!(r["end"], 8);
    }

    #[test]
    fn quote_gone_entirely_is_moved() {
        let d = idx(r#"<p data-cfy-id="sec-a">completely rewritten</p>"#);
        let a = text_anchor("sec-a", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn quote_only_anchor_resolving_needs_no_rewrite() {
        let d = idx(r#"<p data-cfy-id="sec-a">alpha beta gamma</p>"#);
        let a = json!({ "v": 1, "type": "text", "quote": { "exact": "beta" } });
        assert_eq!(d.reattach(&a), Outcome::Anchored { rewritten: None });
    }

    #[test]
    fn offsets_are_utf16_code_units() {
        // v2 text "𝒳𝒳y beta": each 𝒳 is 2 UTF-16 units, so "beta" sits at
        // [6, 10) in UTF-16 (chars would say 4, UTF-8 bytes would say 10).
        // The stale v1 offsets [4, 8) force the in-element re-quote.
        let d = idx("<p data-cfy-id=\"sec-u\">\u{1D4B3}\u{1D4B3}y beta</p>");
        let a = text_anchor("sec-u", 4, 8, "beta");
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["start"], 6);
        assert_eq!(r["end"], 10);
    }

    #[test]
    fn extra_capture_fields_survive_a_rewrite() {
        let d = idx(r#"<p data-cfy-id="sec-a">intro alpha beta gamma</p>"#);
        let mut a = text_anchor("sec-a", 6, 10, "beta");
        a.as_object_mut()
            .unwrap()
            .insert("captured_rect".into(), json!({ "x": 1 }));
        a.as_object_mut().unwrap().insert("target".into(), json!({
            "kind": "code", "label": "borrow example", "excerpt": "beta",
            "cfy_ids": ["sec-a", "code-a"], "multi_block": true
        }));
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["captured_rect"]["x"], 1);
        assert_eq!(r["target"]["kind"], "code");
        assert_eq!(r["target"]["multi_block"], true);
    }

    // -- element anchors ------------------------------------------------------

    #[test]
    fn element_anchor_surviving_id_is_trivially_anchored() {
        let d = idx(
            r#"<figure data-cfy-id="fig-x"><svg><g data-cfy-id="fig-x.node"><text>Node Label</text></g></svg></figure>"#,
        );
        let a = json!({ "v": 1, "type": "element", "cfy_id": "fig-x.node",
                        "quote": { "exact": "Node Label" } });
        assert_eq!(d.reattach(&a), Outcome::Anchored { rewritten: None });
    }

    #[test]
    fn element_anchor_vanished_id_reattaches_by_collapsed_text() {
        // The id was (wrongly) renamed; the node's text survives — note the
        // whitespace difference, absorbed by the collapse convention.
        let d = idx(
            r#"<svg><g data-cfy-id="fig-x.node-v2"><text>Node
                Label</text></g></svg>"#,
        );
        let a = json!({ "v": 1, "type": "element", "cfy_id": "fig-x.node",
                        "quote": { "exact": "Node Label" } });
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "fig-x.node-v2");
        assert_eq!(r["quote"]["exact"], "Node Label");
    }

    #[test]
    fn element_anchor_nested_chain_picks_innermost() {
        // A figure wrapper and its label group collapse to the same text — a
        // strict nesting chain is not ambiguous; the innermost wins.
        let d = idx(
            r#"<figure data-cfy-id="fig-y"> <g data-cfy-id="fig-y.label">Token Service</g> </figure>"#,
        );
        let a = json!({ "v": 1, "type": "element", "cfy_id": "gone.node",
                        "quote": { "exact": "Token Service" } });
        let r = rewritten(d.reattach(&a));
        assert_eq!(r["cfy_id"], "fig-y.label");
    }

    #[test]
    fn element_anchor_parallel_text_matches_are_moved() {
        // Two unrelated elements with identical text: ambiguous.
        let d = idx(
            r#"<g data-cfy-id="a.n">Retry</g><g data-cfy-id="b.n">Retry</g>"#,
        );
        let a = json!({ "v": 1, "type": "element", "cfy_id": "gone.node",
                        "quote": { "exact": "Retry" } });
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn element_anchor_vanished_without_quote_is_moved() {
        // Purely graphical node (no quote captured): nothing to search with.
        let d = idx(r#"<g data-cfy-id="other.node"><rect/></g>"#);
        let a = json!({ "v": 1, "type": "element", "cfy_id": "gone.node" });
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn element_anchor_vanished_with_no_text_match_is_moved() {
        let d = idx(r#"<g data-cfy-id="other.node">Different Text</g>"#);
        let a = json!({ "v": 1, "type": "element", "cfy_id": "gone.node",
                        "quote": { "exact": "Node Label" } });
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    // -- robustness ------------------------------------------------------------

    #[test]
    fn unparseable_anchor_is_flagged_not_dropped() {
        let d = idx(r#"<p data-cfy-id="sec-a">alpha</p>"#);
        assert_eq!(
            d.reattach(&json!({ "v": 1, "type": "wormhole" })),
            Outcome::Moved
        );
        assert_eq!(d.reattach(&json!("not an object")), Outcome::Moved);
    }

    #[test]
    fn empty_document_moves_everything() {
        let d = DocumentIndex::parse("");
        let a = text_anchor("sec-a", 0, 4, "beta");
        assert_eq!(d.reattach(&a), Outcome::Moved);
    }

    #[test]
    fn duplicate_ids_resolve_to_first_in_document_order() {
        // W-ID-DUP documents still save; mirror the bridge's querySelector
        // (first match) so shell and server agree.
        let d = idx(
            r#"<p data-cfy-id="dup">alpha beta gamma</p><p data-cfy-id="dup">other text</p>"#,
        );
        let a = text_anchor("dup", 6, 10, "beta");
        assert_eq!(d.reattach(&a), Outcome::Anchored { rewritten: None });
    }
}
