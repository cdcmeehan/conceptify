//! Local, provider-neutral skill capability catalog and deterministic
//! recommendation service. Discovery reads only local sidecars; draft question
//! text never leaves the process merely to select a skill.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const BUNDLED_CONCEPTIFY: &str = include_str!("../../skill/capabilities.json");
const SIDECAR: &str = "capabilities.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CompatibleResponseControls {
    pub depth: Vec<String>,
    pub language: Vec<String>,
    pub visuals: Vec<String>,
    pub shape: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RecommendationSignals {
    pub terms: Vec<String>,
    pub visual_preference_score: u32,
    pub shape_scores: BTreeMap<String, u32>,
    pub minimum_score: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillCapabilityMetadata {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub outcome: String,
    pub supported_intents: Vec<String>,
    pub context_requirements: Vec<String>,
    pub expected_outputs: Vec<String>,
    pub latency_hint: String,
    pub compatible_response_controls: CompatibleResponseControls,
    pub recommendation: RecommendationSignals,
    pub manual_selectable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillAvailability {
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillCatalogEntry {
    #[serde(flatten)]
    pub metadata: SkillCapabilityMetadata,
    pub availability: SkillAvailability,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResponseIntentInput {
    pub version: u32,
    pub depth: String,
    pub language: String,
    pub visuals: String,
    pub shape: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillRecommendation {
    pub skill: SkillCatalogEntry,
    pub score: u32,
    pub reason: String,
    pub selected_manually: bool,
}

fn default_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![home.join(".claude/skills"), home.join(".codex/skills")]
}

fn validate(metadata: &SkillCapabilityMetadata) -> Result<(), String> {
    if metadata.schema_version != 1 {
        return Err(format!(
            "unsupported capability schema {}",
            metadata.schema_version
        ));
    }
    if metadata.id.is_empty()
        || !metadata
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("skill id must use lowercase letters, digits, '-' or '_'".to_owned());
    }
    if metadata.name.trim().is_empty() || metadata.outcome.trim().is_empty() {
        return Err("skill name and outcome are required".to_owned());
    }
    if !matches!(
        metadata.latency_hint.as_str(),
        "fast" | "moderate" | "extended"
    ) {
        return Err("latency_hint must be fast, moderate, or extended".to_owned());
    }
    if metadata.recommendation.minimum_score == 0 {
        return Err("minimum_score must be positive".to_owned());
    }
    Ok(())
}

fn parse_metadata(raw: &str) -> Result<SkillCapabilityMetadata, String> {
    let metadata: SkillCapabilityMetadata = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    validate(&metadata)?;
    Ok(metadata)
}

fn installed_at(roots: &[PathBuf], id: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(id))
        .find(|dir| dir.join("SKILL.md").is_file())
}

fn discovered_sidecars(roots: &[PathBuf]) -> Vec<(SkillCapabilityMetadata, PathBuf)> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(root) else {
            continue;
        };
        for dir in dirs.flatten() {
            let path = dir.path();
            let sidecar = path.join(SIDECAR);
            let Ok(raw) = std::fs::read_to_string(sidecar) else {
                continue;
            };
            if let Ok(metadata) = parse_metadata(&raw) {
                found.push((metadata, path));
            }
        }
    }
    found
}

fn catalog_from_roots(roots: &[PathBuf]) -> Vec<SkillCatalogEntry> {
    let bundled = parse_metadata(BUNDLED_CONCEPTIFY).expect("bundled capability metadata is valid");
    let mut catalog = BTreeMap::new();
    let installed = installed_at(roots, &bundled.id);
    catalog.insert(
        bundled.id.clone(),
        SkillCatalogEntry {
            metadata: bundled,
            availability: SkillAvailability {
                available: installed.is_some(),
                reason: installed.is_none().then(|| {
                    "Not installed for a supported agent; run `just install-skill` from Conceptify."
                        .to_owned()
                }),
            },
        },
    );

    for (metadata, directory) in discovered_sidecars(roots) {
        let available = directory.join("SKILL.md").is_file();
        catalog.insert(
            metadata.id.clone(),
            SkillCatalogEntry {
                metadata,
                availability: SkillAvailability {
                    available,
                    reason: (!available)
                        .then(|| "Capability metadata exists, but SKILL.md is missing.".to_owned()),
                },
            },
        );
    }
    catalog.into_values().collect()
}

fn validates_intent(intent: &ResponseIntentInput) -> Result<(), String> {
    if intent.version != 1 {
        return Err(format!(
            "unsupported response intent version {}",
            intent.version
        ));
    }
    for (dimension, value, allowed) in [
        (
            "depth",
            intent.depth.as_str(),
            &["quick", "balanced", "deep"][..],
        ),
        (
            "language",
            intent.language.as_str(),
            &["plain", "familiar", "domain_native"][..],
        ),
        (
            "visuals",
            intent.visuals.as_str(),
            &["auto", "prefer", "avoid"][..],
        ),
        (
            "shape",
            intent.shape.as_str(),
            &["auto", "walkthrough", "comparison", "reference"][..],
        ),
    ] {
        if !allowed.contains(&value) {
            return Err(format!("unknown {dimension} value '{value}'"));
        }
    }
    Ok(())
}

fn compatible(entry: &SkillCatalogEntry, intent: &ResponseIntentInput) -> bool {
    let controls = &entry.metadata.compatible_response_controls;
    controls.depth.contains(&intent.depth)
        && controls.language.contains(&intent.language)
        && controls.visuals.contains(&intent.visuals)
        && controls.shape.contains(&intent.shape)
}

fn recommendation_reason(
    entry: &SkillCatalogEntry,
    signals: &[String],
    intent: &ResponseIntentInput,
) -> String {
    if !entry.availability.available {
        return entry
            .availability
            .reason
            .clone()
            .unwrap_or_else(|| "This skill is unavailable.".to_owned());
    }
    if intent.visuals == "prefer" {
        return format!("You prefer visuals; {}", entry.metadata.outcome);
    }
    if signals.is_empty() {
        return format!("Creates the requested outcome: {}", entry.metadata.outcome);
    }
    format!("Matches {}. {}", signals.join(", "), entry.metadata.outcome)
}

fn recommend_from_catalog(
    catalog: Vec<SkillCatalogEntry>,
    question: &str,
    intent: &ResponseIntentInput,
    selected_skill_ids: &[String],
) -> Result<Vec<SkillRecommendation>, String> {
    validates_intent(intent)?;
    let selected: BTreeSet<&str> = selected_skill_ids.iter().map(String::as_str).collect();
    let question = question.to_lowercase();
    let mut recommendations = Vec::new();

    for entry in catalog {
        let manual = selected.contains(entry.metadata.id.as_str());
        if manual && !entry.metadata.manual_selectable {
            return Err(format!(
                "skill '{}' cannot be selected manually",
                entry.metadata.id
            ));
        }
        let mut score = 0;
        let mut signals = Vec::new();
        for term in &entry.metadata.recommendation.terms {
            if question.contains(&term.to_lowercase()) {
                score += 1;
                signals.push(format!("“{term}”"));
            }
        }
        if intent.visuals == "prefer" {
            score += entry.metadata.recommendation.visual_preference_score;
        }
        score += entry
            .metadata
            .recommendation
            .shape_scores
            .get(&intent.shape)
            .copied()
            .unwrap_or(0);

        let automatically_recommended = entry.availability.available
            && compatible(&entry, intent)
            && score >= entry.metadata.recommendation.minimum_score;
        if manual || automatically_recommended {
            let reason = if manual && !entry.availability.available {
                format!(
                    "Selected manually, but unavailable: {}",
                    entry
                        .availability
                        .reason
                        .as_deref()
                        .unwrap_or("installation was not found")
                )
            } else if manual {
                format!("Selected manually. {}", entry.metadata.outcome)
            } else {
                recommendation_reason(&entry, &signals, intent)
            };
            recommendations.push(SkillRecommendation {
                skill: entry,
                score,
                reason,
                selected_manually: manual,
            });
        }
    }
    recommendations.sort_by(|a, b| {
        b.selected_manually
            .cmp(&a.selected_manually)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.skill.metadata.name.cmp(&b.skill.metadata.name))
    });
    Ok(recommendations)
}

#[tauri::command]
pub fn list_skill_capabilities() -> Vec<SkillCatalogEntry> {
    catalog_from_roots(&default_roots())
}

#[tauri::command(rename_all = "snake_case")]
pub fn recommend_skills(
    question: String,
    intent: ResponseIntentInput,
    selected_skill_ids: Vec<String>,
) -> Result<Vec<SkillRecommendation>, String> {
    recommend_from_catalog(
        catalog_from_roots(&default_roots()),
        &question,
        &intent,
        &selected_skill_ids,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(visuals: &str, shape: &str) -> ResponseIntentInput {
        ResponseIntentInput {
            version: 1,
            depth: "balanced".to_owned(),
            language: "familiar".to_owned(),
            visuals: visuals.to_owned(),
            shape: shape.to_owned(),
        }
    }

    fn installed_root() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("conceptify-skills-{}", uuid::Uuid::new_v4()));
        let skill = root.join("conceptify");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# test").unwrap();
        (root, skill)
    }

    #[test]
    fn bundled_metadata_is_valid_and_expressive() {
        let metadata = parse_metadata(BUNDLED_CONCEPTIFY).unwrap();
        assert_eq!(metadata.schema_version, 1);
        assert!(metadata.supported_intents.contains(&"visualize".to_owned()));
        assert!(metadata.expected_outputs.len() >= 2);
        assert!(metadata
            .compatible_response_controls
            .depth
            .contains(&"deep".to_owned()));
    }

    #[test]
    fn ordinary_question_needs_no_skill() {
        let (root, _) = installed_root();
        let result = recommend_from_catalog(
            catalog_from_roots(std::slice::from_ref(&root)),
            "What is ownership?",
            &intent("auto", "auto"),
            &[],
        )
        .unwrap();
        assert!(result.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn visual_intent_gets_local_explainable_recommendation() {
        let (root, _) = installed_root();
        let result = recommend_from_catalog(
            catalog_from_roots(std::slice::from_ref(&root)),
            "Show the request lifecycle",
            &intent("prefer", "walkthrough"),
            &[],
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].skill.metadata.id, "conceptify");
        assert!(result[0].reason.contains("prefer visuals"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unavailable_and_manual_selection_are_explained() {
        let root =
            std::env::temp_dir().join(format!("conceptify-empty-skills-{}", uuid::Uuid::new_v4()));
        let catalog = catalog_from_roots(std::slice::from_ref(&root));
        assert!(!catalog[0].availability.available);
        assert!(catalog[0]
            .availability
            .reason
            .as_deref()
            .unwrap()
            .contains("install-skill"));
        let result = recommend_from_catalog(
            catalog,
            "ordinary question",
            &intent("auto", "auto"),
            &["conceptify".to_owned()],
        )
        .unwrap();
        assert!(result[0].selected_manually);
        assert!(result[0].reason.contains("unavailable"));
    }

    #[test]
    fn future_sidecar_is_discovered_without_service_changes() {
        let (root, _) = installed_root();
        let future = root.join("future-map");
        std::fs::create_dir_all(&future).unwrap();
        std::fs::write(future.join("SKILL.md"), "# future").unwrap();
        let raw = BUNDLED_CONCEPTIFY
            .replace("\"conceptify\"", "\"future-map\"")
            .replace("\"Conceptify artifact\"", "\"Future map\"");
        std::fs::write(future.join(SIDECAR), raw).unwrap();
        let catalog = catalog_from_roots(std::slice::from_ref(&root));
        assert!(catalog
            .iter()
            .any(|entry| entry.metadata.id == "future-map"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
