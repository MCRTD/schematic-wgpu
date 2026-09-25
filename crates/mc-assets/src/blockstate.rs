//! Blockstate definitions: `assets/<ns>/blockstates/<block>.json`.

use std::collections::BTreeMap;

use serde::Deserialize;

pub type Properties = BTreeMap<String, String>;

/// One model reference inside a blockstate, with its whole-block rotation.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Variant {
    pub model: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub uvlock: bool,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// A single variant, or a weighted random choice between several.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VariantList {
    One(Variant),
    Many(Vec<Variant>),
}

impl VariantList {
    pub fn as_slice(&self) -> &[Variant] {
        match self {
            VariantList::One(v) => std::slice::from_ref(v),
            VariantList::Many(v) => v,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    Or {
        #[serde(rename = "OR")]
        or: Vec<Condition>,
    },
    And {
        #[serde(rename = "AND")]
        and: Vec<Condition>,
    },
    /// Every listed property must match; values may be `a|b` alternatives.
    Properties(BTreeMap<String, PropertyValue>),
}

/// Blockstate files use strings, but some packs write booleans or numbers.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    String(String),
    Bool(bool),
    Number(i64),
}

impl PropertyValue {
    fn matches(&self, actual: &str) -> bool {
        match self {
            PropertyValue::String(s) => s.split('|').any(|alt| alt == actual),
            PropertyValue::Bool(b) => actual == if *b { "true" } else { "false" },
            PropertyValue::Number(n) => actual == n.to_string(),
        }
    }
}

impl Condition {
    pub fn matches(&self, props: &Properties) -> bool {
        match self {
            Condition::Or { or } => or.iter().any(|c| c.matches(props)),
            Condition::And { and } => and.iter().all(|c| c.matches(props)),
            Condition::Properties(map) => map
                .iter()
                .all(|(k, v)| props.get(k).is_some_and(|actual| v.matches(actual))),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MultipartCase {
    #[serde(default)]
    pub when: Option<Condition>,
    pub apply: VariantList,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BlockStateDef {
    Variants { variants: BTreeMap<String, VariantList> },
    Multipart { multipart: Vec<MultipartCase> },
}

impl BlockStateDef {
    /// All variant lists that apply to a block with these properties.
    ///
    /// For `variants`, keys must agree with every property the block has.
    /// Properties the block lacks are ignored, which keeps schematics saved
    /// by older versions working after new properties were added; the key
    /// matching the most properties wins.
    pub fn select(&self, props: &Properties) -> Vec<&VariantList> {
        match self {
            BlockStateDef::Variants { variants } => {
                let score = |key: &str| -> Option<usize> {
                    let mut matched = 0;
                    for pair in key.split(',').filter(|p| !p.is_empty()) {
                        let (k, v) = pair.split_once('=')?;
                        match props.get(k) {
                            Some(actual) if actual == v => matched += 1,
                            Some(_) => return None,
                            None => {}
                        }
                    }
                    Some(matched)
                };
                variants
                    .iter()
                    .filter_map(|(key, list)| score(key).map(|s| (s, list)))
                    // max_by_key keeps the last maximum; reverse so ties go to the first key.
                    .rev()
                    .max_by_key(|(s, _)| *s)
                    .map(|(_, list)| list)
                    .into_iter()
                    .collect()
            }
            BlockStateDef::Multipart { multipart } => multipart
                .iter()
                .filter(|case| case.when.as_ref().is_none_or(|c| c.matches(props)))
                .map(|case| &case.apply)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn models(def: &BlockStateDef, p: &Properties) -> Vec<String> {
        def.select(p).iter().map(|l| l.as_slice()[0].model.clone()).collect()
    }

    #[test]
    fn variants_pick_exact_match() {
        let def: BlockStateDef = serde_json::from_str(
            r#"{"variants": {
                "facing=east,half=bottom": {"model": "a"},
                "facing=north,half=bottom": {"model": "b", "y": 270, "uvlock": true},
                "facing=north,half=top": {"model": "c", "x": 180}
            }}"#,
        )
        .unwrap();
        let p = props(&[("facing", "north"), ("half", "bottom"), ("waterlogged", "false")]);
        let lists = def.select(&p);
        assert_eq!(lists.len(), 1);
        let v = &lists[0].as_slice()[0];
        assert_eq!((v.model.as_str(), v.y, v.uvlock), ("b", 270, true));
    }

    #[test]
    fn variants_tolerate_missing_properties() {
        let def: BlockStateDef =
            serde_json::from_str(r#"{"variants": {"axis=x": {"model": "x"}, "axis=y": {"model": "y"}}}"#)
                .unwrap();
        assert_eq!(models(&def, &props(&[("axis", "y")])), ["y"]);
        // No axis property at all: fall back to the first entry.
        assert_eq!(models(&def, &props(&[])), ["x"]);
    }

    #[test]
    fn multipart_conditions() {
        let def: BlockStateDef = serde_json::from_str(
            r#"{"multipart": [
                {"apply": {"model": "post"}},
                {"when": {"north": "side|up"}, "apply": {"model": "n"}},
                {"when": {"OR": [{"east": "up"}, {"west": "up"}]}, "apply": {"model": "ew"}},
                {"when": {"AND": [{"up": true}, {"power": 0}]}, "apply": {"model": "and"}}
            ]}"#,
        )
        .unwrap();
        let p = props(&[("north", "up"), ("east", "none"), ("west", "up"), ("up", "true"), ("power", "0")]);
        assert_eq!(models(&def, &p), ["post", "n", "ew", "and"]);
        let p = props(&[("north", "none"), ("east", "none"), ("west", "side")]);
        assert_eq!(models(&def, &p), ["post"]);
    }

    #[test]
    fn weighted_variant_list() {
        let def: BlockStateDef = serde_json::from_str(
            r#"{"variants": {"": [{"model": "a"}, {"model": "b", "weight": 3}]}}"#,
        )
        .unwrap();
        let lists = def.select(&props(&[]));
        let weights: Vec<u32> = lists[0].as_slice().iter().map(|v| v.weight).collect();
        assert_eq!(weights, [1, 3]);
    }
}
