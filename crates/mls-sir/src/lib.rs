//! Validated statement intermediate representations, canonical hashes, and diffs.

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt, fmt::Write as _};

/// A statement category supported by the statement SIR.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementKind {
    /// A mathematical definition.
    Definition,
    /// A theorem statement.
    Theorem,
    /// A lemma statement.
    Lemma,
    /// A proposition statement.
    Proposition,
    /// A corollary statement.
    Corollary,
}

/// A named, semi-formally typed variable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variable {
    /// The variable's source-level name.
    pub name: String,
    /// A semi-formal description of its type.
    #[serde(rename = "type")]
    pub type_description: String,
}

/// The exact wire representation used while validating deserialization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSir {
    kind: StatementKind,
    #[serde(deserialize_with = "deserialize_label")]
    label: Option<String>,
    variables: Vec<Variable>,
    hypotheses: Vec<String>,
    conclusion: String,
    uses: Vec<String>,
    assumptions_made: Vec<String>,
}

fn deserialize_label<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

/// A validated semi-formal description of a definition or theorem-family claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Sir {
    kind: StatementKind,
    label: Option<String>,
    variables: Vec<Variable>,
    hypotheses: Vec<String>,
    conclusion: String,
    uses: Vec<String>,
    assumptions_made: Vec<String>,
}

impl<'de> Deserialize<'de> for Sir {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSir::deserialize(deserializer)?;
        Self::try_from(raw).map_err(de::Error::custom)
    }
}

impl TryFrom<RawSir> for Sir {
    type Error = ValidationErrors;

    fn try_from(raw: RawSir) -> Result<Self, Self::Error> {
        let errors = validate_raw(&raw);
        if errors.is_empty() {
            Ok(Self {
                kind: raw.kind,
                label: raw.label,
                variables: raw.variables,
                hypotheses: raw.hypotheses,
                conclusion: raw.conclusion,
                uses: raw.uses,
                assumptions_made: raw.assumptions_made,
            })
        } else {
            Err(ValidationErrors(errors))
        }
    }
}

impl Sir {
    /// Construct a SIR and return all content-validation errors in schema order.
    pub fn try_new(
        kind: StatementKind,
        label: Option<String>,
        variables: Vec<Variable>,
        hypotheses: Vec<String>,
        conclusion: String,
        uses: Vec<String>,
        assumptions_made: Vec<String>,
    ) -> Result<Self, ValidationErrors> {
        Self::try_from(RawSir {
            kind,
            label,
            variables,
            hypotheses,
            conclusion,
            uses,
            assumptions_made,
        })
    }

    /// Parse and validate a SIR JSON document.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Return the statement kind.
    #[must_use]
    pub const fn kind(&self) -> StatementKind {
        self.kind
    }

    /// Return the exact source label, or `None` for an explicitly unlabeled statement.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Return variables in meaningful source order.
    #[must_use]
    pub fn variables(&self) -> &[Variable] {
        &self.variables
    }

    /// Return hypotheses in meaningful source order.
    #[must_use]
    pub fn hypotheses(&self) -> &[String] {
        &self.hypotheses
    }

    /// Return the semi-formal conclusion.
    #[must_use]
    pub fn conclusion(&self) -> &str {
        &self.conclusion
    }

    /// Return referenced labels in meaningful source order.
    #[must_use]
    pub fn uses(&self) -> &[String] {
        &self.uses
    }

    /// Return surfaced assumptions, including an explicitly empty list.
    #[must_use]
    pub fn assumptions_made(&self) -> &[String] {
        &self.assumptions_made
    }

    /// Compute a stable SHA-256 over the normalized semantic representation.
    #[must_use]
    pub fn semantic_hash(&self) -> String {
        let canonical = serde_json::to_vec(&self.normalized())
            .expect("serializing a SIR to memory cannot fail");
        let digest = Sha256::digest(canonical);
        digest
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                write!(output, "{byte:02x}").expect("writing to a String cannot fail");
                output
            })
    }

    /// Compare fields after safe whitespace normalization.
    #[must_use]
    pub fn diff(&self, other: &Self) -> SirDiff {
        let left = self.normalized();
        let right = other.normalized();
        let mut changes = Vec::new();
        macro_rules! changed {
            ($name:literal, $field:ident) => {
                if left.$field != right.$field {
                    changes.push(FieldChange {
                        field: $name,
                        before: serde_json::to_value(&left.$field)
                            .expect("SIR fields are serializable"),
                        after: serde_json::to_value(&right.$field)
                            .expect("SIR fields are serializable"),
                    });
                }
            };
        }
        changed!("kind", kind);
        changed!("label", label);
        changed!("variables", variables);
        changed!("hypotheses", hypotheses);
        changed!("conclusion", conclusion);
        changed!("uses", uses);
        changed!("assumptions_made", assumptions_made);
        SirDiff { changes }
    }

    /// Extract conservative structural expectations for a future Lean worker.
    #[must_use]
    pub fn statement_shape(&self) -> StatementShape {
        StatementShape {
            variable_binder_count: self.variables.len(),
            hypothesis_count: self.hypotheses.len(),
        }
    }

    fn normalized(&self) -> RawSir {
        RawSir {
            kind: self.kind,
            label: self.label.clone(),
            variables: self
                .variables
                .iter()
                .map(|variable| Variable {
                    name: variable.name.clone(),
                    type_description: normalize_semiformal(&variable.type_description),
                })
                .collect(),
            hypotheses: normalize_list(&self.hypotheses),
            conclusion: normalize_semiformal(&self.conclusion),
            uses: self.uses.clone(),
            assumptions_made: normalize_list(&self.assumptions_made),
        }
    }
}

/// One deterministic field-level semantic difference.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldChange {
    /// Schema field name.
    pub field: &'static str,
    /// Normalized value before the change.
    pub before: Value,
    /// Normalized value after the change.
    pub after: Value,
}

/// Deterministic differences ordered by schema field order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SirDiff {
    /// Changed fields and their normalized values.
    pub changes: Vec<FieldChange>,
}

impl SirDiff {
    /// Whether the two SIRs are semantically equal under safe whitespace normalization.
    #[must_use]
    pub fn is_cosmetic(&self) -> bool {
        self.changes.is_empty()
    }
}

/// A machine-readable validation problem for one schema path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    /// JSON-style path to the invalid field.
    pub field: String,
    /// Stable machine-readable category.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
}

/// All validation errors in deterministic schema and list order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors(pub Vec<ValidationError>);

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "{} [{}]: {}",
                error.field, error.code, error.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Conservative expectations that can be compared with elaborated Lean structure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatementShape {
    /// Number of explicit variable entries in the SIR.
    pub variable_binder_count: usize,
    /// Number of explicit hypothesis entries in the SIR.
    pub hypothesis_count: usize,
}

fn validate_raw(raw: &RawSir) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    if let Some(label) = &raw.label {
        non_empty("label", label, &mut errors);
    }
    let mut variable_names = HashSet::new();
    for (index, variable) in raw.variables.iter().enumerate() {
        non_empty(
            &format!("variables[{index}].name"),
            &variable.name,
            &mut errors,
        );
        non_empty(
            &format!("variables[{index}].type"),
            &variable.type_description,
            &mut errors,
        );
        if !variable.name.trim().is_empty() && !variable_names.insert(variable.name.clone()) {
            errors.push(ValidationError {
                field: format!("variables[{index}].name"),
                code: "duplicate",
                message: "variable names must be unique".into(),
            });
        }
    }
    validate_list("hypotheses", &raw.hypotheses, false, &mut errors);
    non_empty("conclusion", &raw.conclusion, &mut errors);
    validate_list("uses", &raw.uses, true, &mut errors);
    validate_list(
        "assumptions_made",
        &raw.assumptions_made,
        false,
        &mut errors,
    );
    errors
}

fn validate_list(field: &str, values: &[String], unique: bool, errors: &mut Vec<ValidationError>) {
    let mut seen = HashSet::new();
    for (index, value) in values.iter().enumerate() {
        let path = format!("{field}[{index}]");
        non_empty(&path, value, errors);
        if unique && !value.trim().is_empty() && !seen.insert(value.clone()) {
            errors.push(ValidationError {
                field: path,
                code: "duplicate",
                message: format!("{field} entries must be unique"),
            });
        }
    }
}

fn non_empty(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if value.trim().is_empty() {
        errors.push(ValidationError {
            field: field.into(),
            code: "empty",
            message: "value must contain non-whitespace text".into(),
        });
    }
}

fn normalize_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| normalize_semiformal(value))
        .collect()
}

fn normalize_semiformal(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut delimiter = None;
    let mut escaped = false;
    let mut pending_space = false;
    for character in value.chars() {
        if let Some(active) = delimiter {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' && active != '`' {
                escaped = true;
            } else if character == active {
                delimiter = None;
            }
        } else if matches!(character, '\'' | '"' | '`') {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            delimiter = Some(character);
            output.push(character);
        } else if character.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.push(character);
        }
    }
    if delimiter.is_some() {
        value.to_owned()
    } else {
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/continuous_bounded.json");

    fn fixture() -> Sir {
        Sir::from_json(FIXTURE).expect("fixture must be valid")
    }

    #[test]
    fn fixture_round_trips_with_exact_schema() {
        let sir = fixture();
        let encoded = serde_json::to_string(&sir).unwrap();
        assert_eq!(serde_json::from_str::<Sir>(&encoded).unwrap(), sir);
        assert_eq!(sir.assumptions_made().len(), 1);
    }

    #[test]
    fn mandatory_and_unknown_fields_are_rejected() {
        let missing = FIXTURE.replace(
            ",\n  \"assumptions_made\": [\"interval is closed (text says 'the interval')\"]",
            "",
        );
        assert!(
            Sir::from_json(&missing)
                .unwrap_err()
                .to_string()
                .contains("assumptions_made")
        );
        let unknown = FIXTURE.replace("\n}", ",\n  \"extra\": true\n}");
        assert!(
            Sir::from_json(&unknown)
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }

    #[test]
    fn invalid_kind_and_empty_content_are_rejected() {
        assert!(Sir::from_json(&FIXTURE.replace("\"theorem\"", "\"proof\"")).is_err());
        let empty = FIXTURE.replace("\"∃ M, ∀ x ∈ [a, b], |f x| ≤ M\"", "\"  \"");
        assert!(
            Sir::from_json(&empty)
                .unwrap_err()
                .to_string()
                .contains("conclusion [empty]")
        );
    }

    #[test]
    fn constructor_returns_structured_errors_in_stable_order() {
        let errors = Sir::try_new(
            StatementKind::Definition,
            Some(" ".into()),
            vec![Variable {
                name: "".into(),
                type_description: " ".into(),
            }],
            vec![],
            "".into(),
            vec![],
            vec![],
        )
        .unwrap_err();
        assert_eq!(
            errors
                .0
                .iter()
                .map(|error| (error.field.as_str(), error.code))
                .collect::<Vec<_>>(),
            [
                ("label", "empty"),
                ("variables[0].name", "empty"),
                ("variables[0].type", "empty"),
                ("conclusion", "empty"),
            ]
        );
    }

    #[test]
    fn exact_duplicate_names_and_uses_are_rejected() {
        let duplicate_name =
            FIXTURE.replace("\"a\", \"type\": \"real\"", "\"f\" , \"type\": \"real\"");
        assert!(
            Sir::from_json(&duplicate_name)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        let duplicate_use = FIXTURE.replace(
            "\"def:continuous\"",
            "\"def:continuous\", \"def:continuous\"",
        );
        assert!(
            Sir::from_json(&duplicate_use)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn diff_is_field_level_and_deterministic() {
        let left = fixture();
        let right = Sir::from_json(
            &FIXTURE
                .replace("\"theorem\"", "\"lemma\"")
                .replace("|f x| ≤ M", "|f x| < M"),
        )
        .unwrap();
        let diff = left.diff(&right);
        assert_eq!(
            diff.changes
                .iter()
                .map(|change| change.field)
                .collect::<Vec<_>>(),
            ["kind", "conclusion"]
        );
        assert!(!diff.is_cosmetic());
    }

    #[test]
    fn safe_whitespace_is_cosmetic_but_operator_changes_are_not() {
        let original = fixture();
        let whitespace = Sir::from_json(&FIXTURE.replace("a ≤ b", "  a   ≤   b  ")).unwrap();
        assert!(original.diff(&whitespace).is_cosmetic());
        assert_eq!(original.semantic_hash(), whitespace.semantic_hash());
        let unequal = Sir::from_json(&FIXTURE.replace("a ≤ b", "a < b")).unwrap();
        assert!(!original.diff(&unequal).is_cosmetic());
        assert_ne!(original.semantic_hash(), unequal.semantic_hash());
    }

    #[test]
    fn identity_and_quoted_whitespace_remain_semantic() {
        let original = fixture();
        let changed_label =
            Sir::from_json(&FIXTURE.replace("thm:cont-bounded", "thm:cont- bounded")).unwrap();
        assert!(!original.diff(&changed_label).is_cosmetic());

        let mut first = serde_json::from_str::<Value>(FIXTURE).unwrap();
        first["conclusion"] = Value::String("s = \"a  b\"".into());
        let first: Sir = serde_json::from_value(first).unwrap();
        let mut second = serde_json::from_str::<Value>(FIXTURE).unwrap();
        second["conclusion"] = Value::String("s = \"a b\"".into());
        let second: Sir = serde_json::from_value(second).unwrap();
        assert_ne!(first.semantic_hash(), second.semantic_hash());
        assert!(!first.diff(&second).is_cosmetic());
    }

    #[test]
    fn label_is_mandatory_but_nullable() {
        let null_label = Sir::from_json(&FIXTURE.replace("\"thm:cont-bounded\"", "null")).unwrap();
        assert_eq!(null_label.label(), None);
        let missing_label = FIXTURE.replace("  \"label\": \"thm:cont-bounded\",\n", "");
        assert!(
            Sir::from_json(&missing_label)
                .unwrap_err()
                .to_string()
                .contains("label")
        );
    }

    #[test]
    fn every_field_and_nested_variable_shape_is_strict() {
        let value = serde_json::from_str::<Value>(FIXTURE).unwrap();
        for field in [
            "kind",
            "label",
            "variables",
            "hypotheses",
            "conclusion",
            "uses",
            "assumptions_made",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Sir>(missing).is_err(), "{field}");
        }

        let mut nested_missing = value.clone();
        nested_missing["variables"][0]
            .as_object_mut()
            .unwrap()
            .remove("type");
        assert!(serde_json::from_value::<Sir>(nested_missing).is_err());
        let mut nested_unknown = value.clone();
        nested_unknown["variables"][0]["domain"] = Value::String("ℝ".into());
        assert!(serde_json::from_value::<Sir>(nested_unknown).is_err());

        for (field, wrong) in [
            ("kind", Value::Null),
            ("variables", Value::String("f".into())),
            ("hypotheses", Value::Null),
            ("conclusion", Value::Bool(true)),
            ("uses", Value::Null),
            ("assumptions_made", Value::Null),
        ] {
            let mut invalid = value.clone();
            invalid[field] = wrong;
            assert!(serde_json::from_value::<Sir>(invalid).is_err(), "{field}");
        }
    }

    #[test]
    fn shape_reports_only_explicit_sir_counts() {
        assert_eq!(
            fixture().statement_shape(),
            StatementShape {
                variable_binder_count: 3,
                hypothesis_count: 2,
            }
        );
    }
}
