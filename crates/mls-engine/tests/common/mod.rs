//! Scripted checkers and model clients shared by the batch pipeline and review acceptance tests.
#![allow(dead_code)]

use mls_core::BlockId;
use mls_engine::{PipelineConfig, StatementChecker};
use mls_lean::{ExtractedDeclaration, Position, ReplDiagnostic, Severity};
use mls_models::{
    CallMetadata, DecisionReply, DecisionRequest, GenerationReply, GenerationRequest, ModelClient,
    ModelError, Usage,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub const GENERATION_MODEL: &str = "anthropic/claude-sonnet-5";
pub const DECISION_MODEL: &str = "typesafe/jev-1.14";

pub fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

pub fn config() -> PipelineConfig {
    PipelineConfig {
        max_attempts: 2,
        formalization_max_tokens: 4096,
    }
}

#[derive(Clone)]
pub enum CheckStep {
    Error(&'static str),
    /// A failed check that also reports one structured Lean diagnostic at the given position.
    ErrorWithDiagnostic(&'static str, Position),
    Extracted(ExtractedDeclaration),
}

pub struct ScriptedChecker {
    pub steps: VecDeque<CheckStep>,
    pub checked_sources: Vec<String>,
    pub finishes: Vec<bool>,
    diagnostics: Vec<ReplDiagnostic>,
}

impl ScriptedChecker {
    pub fn new(steps: impl IntoIterator<Item = CheckStep>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            checked_sources: Vec::new(),
            finishes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

impl StatementChecker for ScriptedChecker {
    fn check(&mut self, _: BlockId, _: &str, source: &str) -> Result<ExtractedDeclaration, String> {
        self.checked_sources.push(source.to_owned());
        self.diagnostics.clear();
        match self.steps.pop_front().expect("unexpected checker call") {
            CheckStep::Error(error) => Err(error.into()),
            CheckStep::ErrorWithDiagnostic(error, position) => {
                self.diagnostics = vec![ReplDiagnostic {
                    severity: Severity::Error,
                    pos: Some(position),
                    end_pos: None,
                    data: error.into(),
                }];
                Err(error.into())
            }
            CheckStep::Extracted(value) => Ok(value),
        }
    }

    fn finish(&mut self, accept: bool) -> Result<(), String> {
        self.finishes.push(accept);
        Ok(())
    }

    fn take_diagnostics(&mut self) -> Vec<ReplDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

pub fn extracted(id: u8, goal_outer_head: &str) -> ExtractedDeclaration {
    ExtractedDeclaration {
        declaration: format!("mls_00000000_0000_4000_8000_{id:012}"),
        kind: "theorem".into(),
        binders: Vec::new(),
        goal: "True".into(),
        goal_outer_head: goal_outer_head.into(),
        axioms: vec!["sorryAx".into()],
        has_sorry: true,
        body: None,
        body_outer_head: None,
    }
}

pub struct CapturingClient {
    pub replies: VecDeque<Result<GenerationReply, ModelError>>,
    pub requests: Vec<GenerationRequest>,
    pub last_sir: Option<Value>,
}

impl ModelClient for CapturingClient {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        self.requests.push(request.clone());
        let mut reply = self
            .replies
            .pop_front()
            .expect("missing synthetic response")?;
        // Keep existing scenario fixtures readable while the request boundary is unified: legacy
        // SIR + Lean fixture pairs are folded into the single recorded combined response.
        if reply.output.get("sir").is_none() && reply.output.get("kind").is_some() {
            let sir = reply.output.clone();
            self.last_sir = Some(sir.clone());
            let lean = if self.replies.front().is_some_and(|reply| {
                reply
                    .as_ref()
                    .is_ok_and(|candidate| candidate.output.get("lean_statement").is_some())
            }) {
                self.replies
                    .pop_front()
                    .expect("peeked response exists")?
                    .output
            } else {
                json!({"lean_statement":"theorem placeholder : True := by sorry","notes":[]})
            };
            let statement = lean
                .get("lean_statement")
                .cloned()
                .unwrap_or(json!("theorem placeholder : True := by sorry"));
            let notes = lean.get("notes").cloned().unwrap_or(json!([]));
            reply.output =
                json!({"sir":sir,"lean_statement":statement,"lean_proof":null,"notes":notes});
        } else if reply.output.get("sir").is_none() && reply.output.get("lean_statement").is_some()
        {
            let lean = reply.output.clone();
            reply.output = json!({"sir":self.last_sir.clone().unwrap_or(json!({"kind":"theorem","label":null,"variables":[],"hypotheses":[],"conclusion":"placeholder","uses":[],"assumptions_made":[]})),"lean_statement":lean["lean_statement"],"lean_proof":null,"notes":lean.get("notes").cloned().unwrap_or(json!([]))});
        }
        Ok(reply)
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        unreachable!("M2 does not make decision calls")
    }
}

pub fn sir(id: u8, kind: &str, label: &str, uses: Vec<&str>, conclusion: &str) -> GenerationReply {
    reply(
        id,
        json!({
            "kind": kind,
            "label": label,
            "variables": [],
            "hypotheses": [],
            "conclusion": conclusion,
            "uses": uses,
            "assumptions_made": []
        }),
    )
}

pub fn lean(statement: &str) -> GenerationReply {
    reply(
        0,
        json!({
            "lean_statement": statement,
            "lean_proof": null,
            "notes": []
        }),
    )
}

pub fn reply(seed: u8, output: Value) -> GenerationReply {
    GenerationReply {
        output,
        metadata: CallMetadata {
            http_status: 200,
            configured_model: GENERATION_MODEL.into(),
            resolved_model: "anthropic/claude-sonnet-5-20261001".into(),
            request_id: Some(format!("synthetic-{seed}")),
            provider: Some("synthetic".into()),
            latency_ms: 0,
            usage: Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                cost: Some(0.01),
            },
            provider_response: json!({"synthetic": true}),
        },
        finish_reason: "stop".into(),
    }
}

/// Accepts every candidate and reports the declaration name the engine asked for. Structure
/// checks against the SIR are then the only evidence, which is what the scenarios vary.
pub struct EchoChecker;

impl StatementChecker for EchoChecker {
    fn check(&mut self, _: BlockId, name: &str, _: &str) -> Result<ExtractedDeclaration, String> {
        Ok(ExtractedDeclaration {
            declaration: name.into(),
            kind: "theorem".into(),
            binders: Vec::new(),
            goal: "True".into(),
            goal_outer_head: "True".into(),
            axioms: vec!["sorryAx".into()],
            has_sorry: true,
            body: None,
            body_outer_head: None,
        })
    }

    fn finish(&mut self, _: bool) -> Result<(), String> {
        Ok(())
    }
}

/// Answers SIR requests from a description of the document, so a scenario can change the document
/// between runs. Block 2 uses block 1 exactly when block 1 still has the label `thm:first`.
pub struct OracleClient {
    /// Current labels of block 1 and block 2.
    pub labels: [String; 2],
    /// Semantic ids of block 1 and block 2. Equal ids give equal SIRs.
    pub semantics: [u8; 2],
}

impl ModelClient for OracleClient {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        let second = request.context.block_id.ends_with("000000000002");
        let index = usize::from(second);
        let uses: Vec<&str> = if second && self.labels[0] == "thm:first" {
            vec!["thm:first"]
        } else {
            Vec::new()
        };
        let seed = if second { 2 } else { 1 };
        Ok(reply(
            seed,
            json!({"sir": {
                "kind": "theorem",
                "label": self.labels[index],
                "variables": [],
                "hypotheses": [],
                "conclusion": format!("claim {}", self.semantics[index]),
                "uses": uses,
                "assumptions_made": []
            }, "lean_statement":"theorem placeholder : True := by sorry", "lean_proof":null, "notes": []}),
        ))
    }

    fn decide(&mut self, _: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        unreachable!("M3 review does not make decision calls")
    }
}
