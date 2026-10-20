//! Narrow Jev decisions for M4: typed, bounded questions whose answers only choose what to
//! highlight for the human or when to wait. Confirmation is never decided here.
//!
//! Each decision names its block, sends only the text its question needs, and returns a
//! [`DecisionOutcome`] (or [`FieldFlag`]s) that keeps Jev's raw evidence for the review card.
//! Text is capped at [`DecisionConfig::max_text_chars`] and cut with [`TRUNCATION_MARKER`]. A
//! decision whose answer would be unsafe on cut text does not ask and takes its fallback instead.
//!
//! Nothing in this module can change review state: it has no handle on `ReviewStore`, and no
//! result type names a confirmed status.

use crate::UsageSummary;
use mls_models::{
    Answer, CallContext, ChoiceAnswer, ChoiceAssessment, DecisionReply, DecisionRequest,
    ModelClient, ModelError, NoulAssessment, ProviderFailure, Question, Usage,
};
use mls_sir::Sir;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Marker that ends text cut by the size cap. It counts toward the cap.
pub const TRUNCATION_MARKER: &str = " [truncated]";

/// Longest candidate label offered to the implicit-link choice.
const MAX_LABEL_CHARS: usize = 200;

/// Longest bracketed interval, in characters, that counts as an interval.
const MAX_BRACKET_CHARS: usize = 40;

/// Characters searched after the word "interval" for its phrase.
const INTERVAL_WINDOW_CHARS: usize = 60;

/// Question ID of every single-question request.
const ANSWER: &str = "answer";

/// Tolerance for rounding in a Jev probability distribution, as in `mls-models`.
const SUM_TOLERANCE: f64 = 0.01;

const PROSE_INSTRUCTIONS: &str = "Classify the block. Choose claim if it asserts a mathematical statement that could be formalized. Choose proof_step if it is one step of an argument for a claim. Choose commentary for anything else.";

const LINK_INSTRUCTIONS: &str = "Which of the listed labels does state.block rely on implicitly, for example by saying 'by the previous lemma'? Choose none unless the block clearly relies on one of them.";

const QUANTIFIER_INSTRUCTIONS: &str = "What is the outermost quantifier of the main claim of state.block? Choose universal for a for-all claim, existential for a there-exists claim, and neither otherwise.";

/// A yes/no prompt: the question and the meanings of its true and false answers.
struct Yn {
    question: &'static str,
    yes: &'static str,
    no: &'static str,
}

const FINISHED: Yn = Yn {
    question: "Is state.block a complete, self-contained piece of writing, rather than a sentence or formula still being typed?",
    yes: "The block is complete.",
    no: "The block is incomplete or still being written.",
};

const SEMANTIC_EDIT: Yn = Yn {
    question: "Compare state.old with state.new. Does the edit change the mathematical meaning, rather than only wording, spacing, formatting or typography?",
    yes: "The edit changes the mathematical meaning.",
    no: "The edit changes only wording, spacing, formatting or typography.",
};

const DIFFERS: Yn = Yn {
    question: "state.sir is a semi-formal record of the claim made by state.block. Does state.sir state a different claim, with a different quantifier, hypothesis or conclusion? Answer no for a faithful reading or a mere rewording.",
    yes: "state.sir states a different claim.",
    no: "state.sir faithfully records the claim of state.block.",
};

/// Size and confidence bounds for every Jev decision.
///
/// The thresholds are provisional, taken from the M2 policy in `docs/research/m2-models.md`.
/// They live here, not in `config/models.json`, because `ModelConfig` rejects unknown fields.
/// Calibrate them on MLS examples before relying on them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionConfig {
    /// Maximum characters of any one text sent to Jev, including the truncation marker.
    pub max_text_chars: usize,
    /// Maximum earlier labels offered to the implicit-link choice.
    pub max_candidate_labels: usize,
    /// Maximum hypotheses checked for one SIR; the rest are reported as unchecked.
    pub max_hypotheses: usize,
    /// Maximum interval phrases checked for one block; the rest are reported as unchecked.
    pub max_interval_phrases: usize,
    /// Confidence and selected-probability threshold for the prose-kind choice.
    pub prose_threshold: f64,
    /// Yes/no threshold for the finished gate.
    pub finished_threshold: f64,
    /// Yes/no threshold for the semantic-edit question.
    pub edit_threshold: f64,
    /// Confidence and selected-probability threshold for implicit links.
    pub link_threshold: f64,
    /// Confidence and selected-probability threshold for field checks.
    pub field_threshold: f64,
    /// Yes/no threshold for the obvious-difference highlight.
    pub differs_threshold: f64,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            max_text_chars: 4_000,
            max_candidate_labels: 64,
            max_hypotheses: 8,
            max_interval_phrases: 4,
            prose_threshold: 0.9,
            finished_threshold: 0.9,
            edit_threshold: 0.9,
            link_threshold: 0.9,
            field_threshold: 0.9,
            differs_threshold: 0.9,
        }
    }
}

impl DecisionConfig {
    /// Reject bounds too small to hold a marker and thresholds outside the usable range.
    ///
    /// Thresholds must leave a representable uncertainty probability after the yes/no
    /// assessment's rounding tolerance. The first usable threshold is `0.5 + f64::EPSILON`:
    /// its low cutoff is `0.5`, leaving `0.5_f64.next_up()` uncertain.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_text_chars < 128 {
            return Err(ConfigError("max_text_chars must be at least 128"));
        }
        if self.max_candidate_labels == 0
            || self.max_hypotheses == 0
            || self.max_interval_phrases == 0
        {
            return Err(ConfigError("count bounds must be positive"));
        }
        let thresholds = [
            self.prose_threshold,
            self.finished_threshold,
            self.edit_threshold,
            self.link_threshold,
            self.field_threshold,
            self.differs_threshold,
        ];
        if thresholds.iter().any(|threshold| {
            !threshold.is_finite() || !(0.5 + f64::EPSILON..=1.0).contains(threshold)
        }) {
            return Err(ConfigError(
                "every threshold must be within (0.5 + f64::EPSILON)..=1.0",
            ));
        }
        Ok(())
    }
}

/// A rejected decision configuration.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("invalid decision configuration: {0}")]
pub struct ConfigError(&'static str);

/// Kind of an untagged prose block.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProseKind {
    /// A mathematical claim that should be formalized.
    Claim,
    /// One step of an argument.
    ProofStep,
    /// Anything else. It takes no action.
    Commentary,
}

/// Whether a block looks finished enough to process.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Finished {
    /// Jev is confident the block is complete.
    Yes,
    /// Jev is confident the block is still being written.
    No,
    /// Jev was not confident, so the caller waits for the debounce.
    Unknown,
}

/// Whether an edit changes meaning, for a block that has no SIR yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    /// The edit may change meaning. This is also the fallback.
    Semantic,
    /// Jev is confident the edit changes only the presentation.
    Cosmetic,
}

/// Why a decision did not use Jev's answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// The call failed (transport, provider or replay), so no answer was obtained.
    Failed,
    /// An answer arrived but lacked evidence, had the wrong shape or the wrong question IDs.
    Malformed,
    /// Jev's answer was below the configured threshold.
    LowConfidence,
    /// The input was too large to ask about without cutting it, so no question was sent.
    Oversized,
    /// There were no candidate labels to choose among, so no question was sent.
    NoCandidates,
    /// The candidate labels could not form a valid question, so no question was sent.
    InvalidInput,
}

/// Whether a value came from Jev or from its fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Jev's confident answer was used.
    Jev,
    /// The documented fallback was used, for the given reason.
    Fallback(FallbackReason),
}

/// Jev's raw answer, kept for the review card and for audit.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Evidence {
    /// Model that served the answer, as the provider reported it.
    pub model: String,
    /// Threshold the answer was assessed against.
    pub threshold: f64,
    /// The answer as Jev returned it.
    pub answer: EvidenceAnswer,
}

/// The answer payload of a [`Evidence`] record.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceAnswer {
    /// A yes/no probability; the probability of yes.
    Noul {
        /// Probability of yes.
        probability: f64,
    },
    /// A choice with its distribution and distribution-concentration confidence.
    Choice {
        /// Selected option.
        choice: String,
        /// Full distribution over the options, if Jev returned one.
        probabilities: Option<BTreeMap<String, f64>>,
        /// Confidence, distinct from the selected probability, if Jev returned one.
        confidence: Option<f64>,
    },
}

/// The result of one decision: the value to act on plus the evidence behind it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionOutcome<T> {
    /// The value to act on: Jev's when confident, otherwise the documented fallback.
    pub value: T,
    /// Whether Jev's answer or a fallback produced `value`.
    pub basis: Basis,
    /// Jev's raw answer, absent when no answer arrived at all.
    pub evidence: Option<Evidence>,
    /// Whether the decision's input was cut to the size cap.
    pub truncated: bool,
}

/// Why a field check flagged a field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagReason {
    /// Jev was confident and disagrees with the SIR.
    Disagrees,
    /// Jev was not confident, or no usable answer arrived.
    Uncertain,
    /// The field was not checked because a bound applied.
    Unchecked,
}

/// A review-card highlight on one SIR field. Flags never confirm or reject anything.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldFlag {
    /// Field the flag names, such as `conclusion.quantifier` or `hypotheses[1].stated`.
    pub field: String,
    /// Why the field is flagged.
    pub reason: FlagReason,
    /// Jev's raw answer for the field, if one arrived.
    pub evidence: Option<Evidence>,
}

/// A highlight that the SIR may obviously differ from the text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiffersFlag {
    /// Jev's probability that the SIR differs, when an answer arrived.
    pub probability: Option<f64>,
}

/// The source of Jev answers. The server plugs in [`ModelDecider`]; tests plug in scripts.
pub trait Decider {
    /// Answer one decisions request, or report why no answer was obtained.
    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError>;
}

/// Adapts a [`ModelClient`] to [`Decider`], rejecting replies from any model but the pinned one.
pub struct ModelDecider<C> {
    client: C,
    pinned_model: String,
}

impl<C> ModelDecider<C> {
    /// Wrap a client. `pinned_model` is the decision model ID from configuration.
    pub fn new(client: C, pinned_model: impl Into<String>) -> Self {
        Self {
            client,
            pinned_model: pinned_model.into(),
        }
    }
}

impl<C: ModelClient> Decider for ModelDecider<C> {
    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        let reply = self.client.decide(request)?;
        if reply.metadata.configured_model != self.pinned_model {
            return Err(ModelError::Provider {
                message: "decision reply was configured for a model other than the pinned one"
                    .into(),
                failure: Box::new(ProviderFailure {
                    status: reply.metadata.http_status,
                    response: reply.metadata.provider_response,
                    resolved_model: Some(reply.metadata.resolved_model),
                    usage: Some(reply.metadata.usage),
                }),
            });
        }
        Ok(reply)
    }
}

/// Runs the M4 decisions through a [`Decider`] and accounts every call made.
pub struct DecisionLayer<D> {
    decider: D,
    config: DecisionConfig,
    usage: UsageSummary,
    attempts: BTreeMap<String, u32>,
}

impl<D: Decider> DecisionLayer<D> {
    /// Create a layer after validating its configuration.
    pub fn new(decider: D, config: DecisionConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            decider,
            config,
            usage: UsageSummary::default(),
            attempts: BTreeMap::new(),
        })
    }

    /// Aggregate usage for every decision call made, including failed calls.
    pub fn usage(&self) -> &UsageSummary {
        &self.usage
    }

    /// Classify an untagged prose block. Cut text is still asked: the fallback is commentary.
    pub fn classify_prose(
        &mut self,
        block_id: &str,
        block_text: &str,
    ) -> DecisionOutcome<ProseKind> {
        let (text, truncated) = bounded(block_text, self.config.max_text_chars);
        let threshold = self.config.prose_threshold;
        let choices = options(&[
            (
                "claim",
                "The block states a mathematical claim to formalize.",
            ),
            (
                "proof_step",
                "The block is one step of an argument for a claim.",
            ),
            (
                "commentary",
                "The block is anything else, such as motivation or remarks.",
            ),
        ]);
        let judged = self.choose(
            block_id,
            PROSE_INSTRUCTIONS,
            choices,
            json!({"block": text}),
            threshold,
        );
        let verdict = judged.verdict.map(|label| match label.as_str() {
            "claim" => ProseKind::Claim,
            "proof_step" => ProseKind::ProofStep,
            _ => ProseKind::Commentary,
        });
        outcome(verdict, ProseKind::Commentary, judged.evidence, truncated)
    }

    /// Decide whether a block is finished. Cut text is never judged finished: the answer is unknown.
    pub fn is_finished(&mut self, block_id: &str, block_text: &str) -> DecisionOutcome<Finished> {
        let (text, truncated) = bounded(block_text, self.config.max_text_chars);
        if truncated {
            return skipped(Finished::Unknown, FallbackReason::Oversized, true);
        }
        let threshold = self.config.finished_threshold;
        let judged = self.judge_yes_no(block_id, json!({"block": text}), &FINISHED, threshold);
        let verdict = judged
            .verdict
            .map(|yes| if yes { Finished::Yes } else { Finished::No });
        outcome(verdict, Finished::Unknown, judged.evidence, false)
    }

    /// Decide whether an edit to a block without a SIR is cosmetic. Cut text is always semantic.
    pub fn is_semantic_edit(
        &mut self,
        block_id: &str,
        old_text: &str,
        new_text: &str,
    ) -> DecisionOutcome<EditKind> {
        let (old, old_cut) = bounded(old_text, self.config.max_text_chars);
        let (new, new_cut) = bounded(new_text, self.config.max_text_chars);
        if old_cut || new_cut {
            return skipped(EditKind::Semantic, FallbackReason::Oversized, true);
        }
        let threshold = self.config.edit_threshold;
        let judged = self.judge_yes_no(
            block_id,
            json!({"old": old, "new": new}),
            &SEMANTIC_EDIT,
            threshold,
        );
        let verdict = judged.verdict.map(|semantic| {
            if semantic {
                EditKind::Semantic
            } else {
                EditKind::Cosmetic
            }
        });
        outcome(verdict, EditKind::Semantic, judged.evidence, false)
    }

    /// Choose the earlier label a block relies on, or `None`. Callers ask Sonnet when this is `None`
    /// because of a fallback. Cut text always falls back: a missing suffix may name a dependency.
    pub fn link_implicit(
        &mut self,
        block_id: &str,
        block_text: &str,
        earlier_labels: &[String],
    ) -> DecisionOutcome<Option<String>> {
        let (text, truncated) = bounded(block_text, self.config.max_text_chars);
        if truncated {
            return skipped(None, FallbackReason::Oversized, true);
        }
        if earlier_labels.is_empty() {
            return skipped(None, FallbackReason::NoCandidates, truncated);
        }
        if earlier_labels.len() > self.config.max_candidate_labels
            || earlier_labels
                .iter()
                .any(|label| label.chars().count() > MAX_LABEL_CHARS)
        {
            return skipped(None, FallbackReason::Oversized, truncated);
        }
        let distinct: BTreeSet<&str> = earlier_labels.iter().map(String::as_str).collect();
        if distinct.len() != earlier_labels.len()
            || distinct.contains("none")
            || earlier_labels.iter().any(String::is_empty)
        {
            return skipped(None, FallbackReason::InvalidInput, truncated);
        }
        let mut choices: BTreeMap<String, String> = earlier_labels
            .iter()
            .map(|label| {
                (
                    label.clone(),
                    format!("The block relies on the statement labelled {label}."),
                )
            })
            .collect();
        choices.insert(
            "none".into(),
            "The block relies on none of the listed statements.".into(),
        );
        let threshold = self.config.link_threshold;
        let judged = self.choose(
            block_id,
            LINK_INSTRUCTIONS,
            choices,
            json!({"block": text, "candidates": earlier_labels}),
            threshold,
        );
        let verdict = judged
            .verdict
            .map(|label| (label != "none").then_some(label));
        outcome(verdict, None, judged.evidence, truncated)
    }

    /// Check each SIR field against the text. A flag marks a field for the reviewer; it never
    /// confirms or rejects. Only disagreement with confident evidence flags a field as
    /// [`FlagReason::Disagrees`]; low confidence or a failed call flags it as `Uncertain`.
    pub fn check_sir_fields(
        &mut self,
        block_id: &str,
        block_text: &str,
        sir: &Sir,
    ) -> Vec<FieldFlag> {
        let (text, truncated) = bounded(block_text, self.config.max_text_chars);
        if truncated {
            return vec![unchecked("block")];
        }
        let max_hypotheses = self.config.max_hypotheses;
        let max_phrases = self.config.max_interval_phrases;
        let threshold = self.config.field_threshold;

        let mut flags = Vec::new();
        let mut checks = vec![Check {
            field: "conclusion.quantifier".into(),
            instructions: QUANTIFIER_INSTRUCTIONS.into(),
            options: options(&[
                (
                    "universal",
                    "The main claim holds for all elements of its domain.",
                ),
                (
                    "existential",
                    "The main claim asserts that some element exists.",
                ),
                ("neither", "The main claim has no outermost quantifier."),
            ]),
            accepted: vec![quantifier_of(sir.conclusion()).into()],
        }];

        let mut hypothesis_texts = Vec::new();
        for (index, hypothesis) in sir.hypotheses().iter().enumerate() {
            let field = format!("hypotheses[{index}].stated");
            if index >= max_hypotheses {
                flags.push(unchecked(&field));
                continue;
            }
            let (hypothesis, cut) = bounded(hypothesis, self.config.max_text_chars);
            if cut {
                flags.push(unchecked(&field));
                continue;
            }
            checks.push(Check {
                field,
                instructions: format!(
                    "Is the hypothesis state.hypotheses[{}] stated explicitly in state.block, or only implied by it?",
                    hypothesis_texts.len()
                ),
                options: options(&[
                    ("stated", "The block states this hypothesis explicitly."),
                    ("implied", "The block only implies this hypothesis."),
                ]),
                accepted: vec!["stated".into()],
            });
            hypothesis_texts.push(hypothesis);
        }

        let sir_kinds = sir_interval_kinds(sir);
        let mut phrase_texts = Vec::new();
        if !sir_kinds.is_empty() {
            for (index, phrase) in interval_phrases(&text).into_iter().enumerate() {
                let field = format!("interval[{index}].endpoints");
                if index >= max_phrases {
                    flags.push(unchecked("interval.endpoints"));
                    break;
                }
                match phrase.kind {
                    // Explicit brackets are read deterministically; only the SIR can disagree.
                    Some(kind) => {
                        if !sir_kinds.contains(kind) {
                            flags.push(FieldFlag {
                                field,
                                reason: FlagReason::Disagrees,
                                evidence: None,
                            });
                        }
                    }
                    None => {
                        checks.push(Check {
                            field,
                            instructions: format!(
                                "Does the interval phrase state.interval_phrases[{}] in state.block include its endpoints?",
                                phrase_texts.len()
                            ),
                            options: options(&[
                                ("closed", "Both endpoints are included."),
                                ("open", "Neither endpoint is included."),
                                ("half_open", "Exactly one endpoint is included."),
                                ("unspecified", "The text does not say which endpoints are included."),
                            ]),
                            accepted: sir_kinds
                                .iter()
                                .map(|kind| (*kind).to_owned())
                                .chain(["unspecified".to_owned()])
                                .collect(),
                        });
                        phrase_texts.push(phrase.text);
                    }
                }
            }
        }

        let mut state = Map::new();
        state.insert("block".into(), json!(text));
        if !hypothesis_texts.is_empty() {
            state.insert("hypotheses".into(), json!(hypothesis_texts));
        }
        if !phrase_texts.is_empty() {
            state.insert("interval_phrases".into(), json!(phrase_texts));
        }
        let questions: BTreeMap<String, Question> = checks
            .iter()
            .map(|check| {
                (
                    check.field.clone(),
                    Question::Choice {
                        instructions: check.instructions.clone(),
                        criteria: check.options.clone(),
                    },
                )
            })
            .collect();

        match self.ask(block_id, questions, Value::Object(state)) {
            Ok(reply) => {
                for check in &checks {
                    let judged = read_choice(&reply, &check.field, &check.options, threshold);
                    let reason = match judged.verdict {
                        Ok(label) if check.accepted.contains(&label) => continue,
                        Ok(_) => FlagReason::Disagrees,
                        Err(_) => FlagReason::Uncertain,
                    };
                    flags.push(FieldFlag {
                        field: check.field.clone(),
                        reason,
                        evidence: judged.evidence,
                    });
                }
            }
            Err(_) => {
                for check in &checks {
                    flags.push(FieldFlag {
                        field: check.field.clone(),
                        reason: FlagReason::Uncertain,
                        evidence: None,
                    });
                }
            }
        }
        flags
    }

    /// Highlight a SIR that obviously differs from its text. `Some` means highlight. Uncertainty
    /// highlights too, as the §6 fallback requires; only a confident "no" leaves it unhighlighted.
    pub fn sir_obviously_differs(
        &mut self,
        block_id: &str,
        block_text: &str,
        sir: &Sir,
    ) -> DecisionOutcome<Option<DiffersFlag>> {
        let (text, block_cut) = bounded(block_text, self.config.max_text_chars);
        let sir_json = json!({
            "kind": sir.kind(),
            "variables": sir.variables(),
            "hypotheses": sir.hypotheses(),
            "conclusion": sir.conclusion(),
        })
        .to_string();
        let (sir_text, sir_cut) = bounded(&sir_json, self.config.max_text_chars);
        if block_cut || sir_cut {
            return skipped(
                Some(DiffersFlag { probability: None }),
                FallbackReason::Oversized,
                true,
            );
        }
        let threshold = self.config.differs_threshold;
        let judged = self.judge_yes_no(
            block_id,
            json!({"block": text, "sir": sir_text}),
            &DIFFERS,
            threshold,
        );
        let flag = DiffersFlag {
            probability: noul_probability(judged.evidence.as_ref()),
        };
        let (value, basis) = match judged.verdict {
            Ok(false) => (None, Basis::Jev),
            Ok(true) => (Some(flag), Basis::Jev),
            Err(reason) => (Some(flag), Basis::Fallback(reason)),
        };
        DecisionOutcome {
            value,
            basis,
            evidence: judged.evidence,
            truncated: false,
        }
    }

    /// Ask one yes/no question and read its answer against the threshold.
    fn judge_yes_no(
        &mut self,
        block_id: &str,
        state: Value,
        prompt: &Yn,
        threshold: f64,
    ) -> Judged<bool> {
        let questions = BTreeMap::from([(
            ANSWER.to_owned(),
            Question::Noul {
                instructions: prompt.question.to_owned(),
                criteria: BTreeMap::from([
                    ("true".to_owned(), prompt.yes.to_owned()),
                    ("false".to_owned(), prompt.no.to_owned()),
                ]),
            },
        )]);
        match self.ask(block_id, questions, state) {
            Ok(reply) => read_noul(&reply, threshold),
            Err(reason) => Judged::unanswered(reason),
        }
    }

    /// Ask one bounded choice and read its answer against the threshold.
    fn choose(
        &mut self,
        block_id: &str,
        instructions: &str,
        options: BTreeMap<String, String>,
        state: Value,
        threshold: f64,
    ) -> Judged<String> {
        let questions = BTreeMap::from([(
            ANSWER.to_owned(),
            Question::Choice {
                instructions: instructions.to_owned(),
                criteria: options.clone(),
            },
        )]);
        match self.ask(block_id, questions, state) {
            Ok(reply) => read_choice(&reply, ANSWER, &options, threshold),
            Err(reason) => Judged::unanswered(reason),
        }
    }

    /// Make one decisions call, count its usage, and check that the answer IDs match.
    ///
    /// Replay misses and stopped recorders never reached a provider, so they are not counted.
    fn ask(
        &mut self,
        block_id: &str,
        questions: BTreeMap<String, Question>,
        state: Value,
    ) -> Result<DecisionReply, FallbackReason> {
        let attempt = self.attempts.entry(block_id.to_owned()).or_default();
        let context = CallContext {
            block_id: block_id.to_owned(),
            attempt: *attempt,
        };
        *attempt += 1;
        let request = DecisionRequest {
            context,
            state,
            questions,
        };
        match self.decider.decide(&request) {
            Ok(reply) => {
                self.usage.add(&reply.metadata.usage);
                if reply.answers.keys().eq(request.questions.keys()) {
                    Ok(reply)
                } else {
                    Err(FallbackReason::Malformed)
                }
            }
            Err(error) => {
                match &error {
                    ModelError::ReplayMismatch | ModelError::RecordingStopped => {}
                    ModelError::Provider { failure, .. } => {
                        self.usage.add(&failure.usage.clone().unwrap_or_default());
                    }
                    _ => self.usage.add(&Usage::default()),
                }
                Err(FallbackReason::Failed)
            }
        }
    }
}

impl<D: Decider> crate::live::FinishedOracle for DecisionLayer<D> {
    fn is_finished(&mut self, query: &crate::live::FinishedQuery<'_>) -> crate::live::OracleAnswer {
        let outcome = DecisionLayer::is_finished(self, &query.block.to_string(), query.text);
        if outcome.basis != Basis::Jev {
            return crate::live::OracleAnswer::UNKNOWN;
        }
        let Some(probability) = noul_probability(outcome.evidence.as_ref()) else {
            return crate::live::OracleAnswer::UNKNOWN;
        };
        match outcome.value {
            Finished::Yes => crate::live::OracleAnswer {
                finished: crate::live::Finished::Yes,
                confidence: probability,
            },
            Finished::No => crate::live::OracleAnswer {
                finished: crate::live::Finished::No,
                confidence: 1.0 - probability,
            },
            Finished::Unknown => crate::live::OracleAnswer::UNKNOWN,
        }
    }
}

/// A decision's verdict before the fallback is applied, with the evidence behind it.
struct Judged<T> {
    verdict: Result<T, FallbackReason>,
    evidence: Option<Evidence>,
}

impl<T> Judged<T> {
    fn unanswered(reason: FallbackReason) -> Self {
        Self {
            verdict: Err(reason),
            evidence: None,
        }
    }
}

/// One field-level question and the labels that count as agreeing with the SIR.
struct Check {
    field: String,
    instructions: String,
    options: BTreeMap<String, String>,
    accepted: Vec<String>,
}

fn options(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(label, meaning)| ((*label).to_owned(), (*meaning).to_owned()))
        .collect()
}

fn outcome<T>(
    verdict: Result<T, FallbackReason>,
    fallback: T,
    evidence: Option<Evidence>,
    truncated: bool,
) -> DecisionOutcome<T> {
    let (value, basis) = match verdict {
        Ok(value) => (value, Basis::Jev),
        Err(reason) => (fallback, Basis::Fallback(reason)),
    };
    DecisionOutcome {
        value,
        basis,
        evidence,
        truncated,
    }
}

fn skipped<T>(fallback: T, reason: FallbackReason, truncated: bool) -> DecisionOutcome<T> {
    DecisionOutcome {
        value: fallback,
        basis: Basis::Fallback(reason),
        evidence: None,
        truncated,
    }
}

fn unchecked(field: &str) -> FieldFlag {
    FieldFlag {
        field: field.to_owned(),
        reason: FlagReason::Unchecked,
        evidence: None,
    }
}

/// Cut `text` to at most `max_chars` characters, ending with [`TRUNCATION_MARKER`] when cut.
fn bounded(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_owned(), false);
    }
    let keep = max_chars - TRUNCATION_MARKER.chars().count();
    let mut cut: String = text.chars().take(keep).collect();
    cut.push_str(TRUNCATION_MARKER);
    (cut, true)
}

fn valid_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn read_noul(reply: &DecisionReply, threshold: f64) -> Judged<bool> {
    let Some(Answer::Noul(answer)) = reply.answers.get(ANSWER) else {
        return Judged::unanswered(FallbackReason::Malformed);
    };
    let evidence = Evidence {
        model: reply.metadata.resolved_model.clone(),
        threshold,
        answer: EvidenceAnswer::Noul {
            probability: answer.noul,
        },
    };
    let verdict = if !valid_probability(answer.noul) {
        Err(FallbackReason::Malformed)
    } else {
        match answer.assess(threshold) {
            NoulAssessment::Yes => Ok(true),
            NoulAssessment::No => Ok(false),
            NoulAssessment::Uncertain => Err(FallbackReason::LowConfidence),
        }
    };
    Judged {
        verdict,
        evidence: Some(evidence),
    }
}

fn read_choice(
    reply: &DecisionReply,
    id: &str,
    options: &BTreeMap<String, String>,
    threshold: f64,
) -> Judged<String> {
    let Some(Answer::Choice(answer)) = reply.answers.get(id) else {
        return Judged::unanswered(FallbackReason::Malformed);
    };
    let evidence = Evidence {
        model: reply.metadata.resolved_model.clone(),
        threshold,
        answer: EvidenceAnswer::Choice {
            choice: answer.choice.clone(),
            probabilities: answer.probabilities.clone(),
            confidence: answer.confidence,
        },
    };
    let verdict = if !well_formed_choice(answer, options) {
        Err(FallbackReason::Malformed)
    } else {
        match answer.assess(options, threshold) {
            ChoiceAssessment::Selected(label) => Ok(label),
            ChoiceAssessment::Uncertain => Err(FallbackReason::LowConfidence),
        }
    };
    Judged {
        verdict,
        evidence: Some(evidence),
    }
}

/// Whether a choice carries the full distribution and confidence that acting on it requires.
fn well_formed_choice(answer: &ChoiceAnswer, options: &BTreeMap<String, String>) -> bool {
    let (Some(probabilities), Some(confidence)) = (&answer.probabilities, answer.confidence) else {
        return false;
    };
    options.contains_key(&answer.choice)
        && probabilities.len() == options.len()
        && probabilities.keys().all(|key| options.contains_key(key))
        && probabilities
            .values()
            .all(|value| valid_probability(*value))
        && valid_probability(confidence)
        && (probabilities.values().sum::<f64>() - 1.0).abs() <= SUM_TOLERANCE
}

fn noul_probability(evidence: Option<&Evidence>) -> Option<f64> {
    match evidence?.answer {
        EvidenceAnswer::Noul { probability } => Some(probability),
        EvidenceAnswer::Choice { .. } => None,
    }
}

fn quantifier_of(conclusion: &str) -> &'static str {
    let conclusion = conclusion.trim_start();
    let mut words = conclusion.split_whitespace();
    if words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("for"))
        && words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("all"))
    {
        return "universal";
    }
    match conclusion.chars().next() {
        Some('∀') => "universal",
        Some('∃') => "existential",
        _ => "neither",
    }
}

/// An interval-like phrase found in block text.
#[derive(Debug, PartialEq)]
struct IntervalPhrase {
    /// Endpoint convention when explicit brackets make it certain; `None` leaves it to Jev.
    kind: Option<&'static str>,
    text: String,
}

/// Find the phrases introduced by the word "interval", deterministically.
fn interval_phrases(text: &str) -> Vec<IntervalPhrase> {
    let chars: Vec<char> = text.chars().collect();
    let word = "interval".len();
    let mut phrases = Vec::new();
    let mut start = 0;
    while start + word <= chars.len() {
        let at_word_start = start == 0 || !chars[start - 1].is_alphanumeric();
        let matches_word = chars[start..start + word]
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case("interval");
        if !(at_word_start && matches_word) {
            start += 1;
            continue;
        }
        let window_start = start + word;
        let window_end = (window_start + INTERVAL_WINDOW_CHARS).min(chars.len());
        let mut bracket = None;
        for index in window_start..window_end {
            if matches!(chars[index], '.' | ';' | '\n') {
                break;
            }
            if let Some((end, kind)) = bracket_at(&chars, index) {
                bracket = Some((index, end, kind));
                break;
            }
        }
        match bracket {
            Some((open, close, kind)) => phrases.push(IntervalPhrase {
                kind: Some(kind),
                text: chars[open..=close].iter().collect(),
            }),
            None => {
                let end = chars[window_start..window_end]
                    .iter()
                    .position(|c| matches!(c, '.' | ';' | '\n'))
                    .map_or(window_end, |offset| window_start + offset);
                phrases.push(IntervalPhrase {
                    kind: None,
                    text: chars[start..end]
                        .iter()
                        .collect::<String>()
                        .trim_end()
                        .to_owned(),
                });
            }
        }
        start += word;
    }
    phrases
}

/// Read a bracketed interval opening at `open`: one comma, no nested brackets, and not a call
/// such as `f(x, y)`. Returns the closing index and the endpoint convention.
fn bracket_at(chars: &[char], open: usize) -> Option<(usize, &'static str)> {
    let opener = chars[open];
    if !matches!(opener, '[' | '(') {
        return None;
    }
    let word = "interval".len();
    let after_interval = open >= word
        && (open == word || !chars[open - word - 1].is_alphanumeric())
        && chars[open - word..open]
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case("interval");
    if open > 0
        && (chars[open - 1].is_alphanumeric() || matches!(chars[open - 1], ')' | ']' | '}'))
        && !after_interval
    {
        return None;
    }
    let search_end = chars.len().min(open + MAX_BRACKET_CHARS + 2);
    let close =
        (open + 1..search_end).find(|&index| matches!(chars[index], ']' | ')' | '[' | '('))?;
    let closer = chars[close];
    if !matches!(closer, ']' | ')') {
        return None;
    }
    let content = &chars[open + 1..close];
    if content.len() > MAX_BRACKET_CHARS || content.iter().filter(|&&c| c == ',').count() != 1 {
        return None;
    }
    let kind = match (opener, closer) {
        ('[', ']') => "closed",
        ('(', ')') => "open",
        _ => "half_open",
    };
    Some((close, kind))
}

/// Endpoint conventions of every bracketed interval written in the SIR.
fn sir_interval_kinds(sir: &Sir) -> BTreeSet<&'static str> {
    let mut texts = vec![sir.conclusion()];
    texts.extend(sir.hypotheses().iter().map(String::as_str));
    texts.extend(sir.variables().iter().map(|v| v.type_description.as_str()));
    texts
        .into_iter()
        .flat_map(|text| {
            let chars: Vec<char> = text.chars().collect();
            (0..chars.len())
                .filter_map(|index| bracket_at(&chars, index).map(|(_, kind)| kind))
                .collect::<Vec<_>>()
        })
        .collect()
}
