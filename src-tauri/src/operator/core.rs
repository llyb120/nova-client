//! Provider-independent interaction state. No imports from Lyra or Reasonix.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_CHECKPOINT_BYTES: usize = 12_000;
pub const MAX_RESULT_BYTES: usize = 24_000;
pub const MAX_OBSERVATION_BYTES: usize = 80_000;
pub const RECENT_ACTIONS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelIdentity {
    pub agent: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub op: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub request_key: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub target: Value,
    #[serde(default)]
    pub inputs: Value,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
}

impl Request {
    pub fn parse(value: &Value) -> Result<Self, String> {
        if value.to_string().len() > 32_000 {
            return Err("operate request exceeds 32 KB".into());
        }
        let r: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if !matches!(
            r.op.as_str(),
            "run" | "resume" | "status" | "result" | "cancel"
        ) {
            return Err("op must be run/resume/status/result/cancel".into());
        }
        if r.op == "run" {
            if r.goal.as_deref().unwrap_or("").trim().is_empty()
                || r.request_key.as_deref().unwrap_or("").trim().is_empty()
            {
                return Err("run requires goal and requestKey".into());
            }
            if !matches!(r.channel.as_deref(), Some("chrome" | "jianlai")) {
                return Err("channel must be chrome or jianlai".into());
            }
            if r.acceptance.is_empty() {
                return Err("run requires explicit acceptance criteria".into());
            }
        } else if r.task_id.as_deref().unwrap_or("").is_empty() {
            return Err("taskId is required".into());
        }
        Ok(r)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decision {
    pub kind: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub checkpoint: Option<Value>,
    #[serde(default)]
    pub evidence_id: Option<String>,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub reason: String,
    /// The model must flag irreversible/business-commit actions. This supplements,
    /// not replaces, caller authorization and native tool snapshot validation.
    #[serde(default)]
    pub requires_confirmation: bool,
}
impl Decision {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > 48_000 {
            return Err("Operator decision exceeds 48 KB".into());
        }
        let text = text.trim();
        let text = if text.starts_with("```json\n") && text.ends_with("```") {
            &text[8..text.len() - 3]
        } else if text.starts_with("```\n") && text.ends_with("```") {
            &text[4..text.len() - 3]
        } else {
            text
        };
        let d: Self =
            serde_json::from_str(text).map_err(|e| format!("Invalid Operator decision: {e}"))?;
        if !matches!(d.kind.as_str(), "observe" | "act" | "finish" | "blocked") {
            return Err("Invalid decision kind".into());
        }
        if d.checkpoint
            .as_ref()
            .is_some_and(|c| c.to_string().len() > MAX_CHECKPOINT_BYTES)
        {
            return Err(
                "Checkpoint exceeds budget; narrow the task instead of dropping progress".into(),
            );
        }
        if d.result.to_string().len() > MAX_RESULT_BYTES {
            return Err("Result exceeds budget; split the task".into());
        }
        Ok(d)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub evidence_id: String,
    pub payload: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRecord {
    pub id: String,
    pub operation: String,
    pub state: String,
    pub evidence_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub version: u32,
    pub id: String,
    pub owner_key: String,
    pub root: String,
    pub model: ModelIdentity,
    pub contract: Request,
    pub status: String,
    pub checkpoint: Value,
    pub current: Option<Observation>,
    #[serde(default)]
    pub last_evidence_id: Option<String>,
    pub actions: Vec<ActionRecord>,
    pub result: Value,
    pub reason: String,
    pub revision: u64,
    pub decisions: u64,
    pub input_text_bytes: u64,
    pub input_images: u64,
}
impl Task {
    pub fn new(
        id: String,
        owner_key: String,
        root: String,
        model: ModelIdentity,
        contract: Request,
    ) -> Self {
        Self {
            version: 1,
            id,
            owner_key,
            root,
            model,
            contract,
            status: "yielded".into(),
            checkpoint: json!({}),
            current: None,
            last_evidence_id: None,
            actions: vec![],
            result: Value::Null,
            reason: String::new(),
            revision: 0,
            decisions: 0,
            input_text_bytes: 0,
            input_images: 0,
        }
    }
    pub fn project(&self, tool_schema: &Value) -> Value {
        // NEVER copy the parent transcript or the full observation archive here.
        // All references from older observations are evidence, not actionable locators.
        json!({"contract":self.contract,"checkpoint":self.checkpoint,
            "recentActions":self.actions.iter().rev().take(RECENT_ACTIONS).collect::<Vec<_>>(),
            "currentObservation":self.current.as_ref().map(|o| json!({"evidenceId":o.evidence_id,"data":without_blobs(&o.payload)})),
            "tool":tool_schema,
            "decisionFormat":{"kind":"observe|act|finish|blocked","params":"native tool arguments","checkpoint":"bounded cumulative facts, sources, coverage and unresolved items","evidenceId":"current observation evidence ID","result":"finish only","reason":"reason / blocked explanation","requiresConfirmation":"true for sending/submitting/purchasing/deleting or other irreversible changes"}})
    }
    pub fn validate_decision(&self, d: &Decision) -> Result<(), String> {
        if d.kind == "blocked" {
            return Ok(());
        }
        if d.kind == "finish" {
            let observation = self
                .current
                .as_ref()
                .ok_or("Observe and verify before finishing")?;
            if observation.payload["snapshotId"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                return Err(
                    "Finish requires a fresh actionable observation, not just a tool status".into(),
                );
            }
            if d.evidence_id.as_deref() != Some(&observation.evidence_id) {
                return Err("Finish must cite the current observation".into());
            }
            if self
                .actions
                .iter()
                .any(|a| a.state == "unknown" || a.state == "dispatched")
            {
                return Err(
                    "An operation is unverified; require user review, not a success claim".into(),
                );
            }
            return Ok(());
        }
        let op = d.params["operation"]
            .as_str()
            .ok_or("Native operation is required")?;
        let allowed_observe = if self.contract.channel.as_deref() == Some("chrome") {
            matches!(op, "tabs" | "inspect" | "screenshot" | "status")
        } else {
            matches!(op, "windows" | "screenshot")
        };
        if d.kind == "observe" {
            if !allowed_observe {
                return Err("Observation cannot perform an action".into());
            }
        } else {
            // Navigation/typing/clicking must all use a guarded act based on observation.
            // No arbitrary URLs, javascript, shell, filesystem, experience writes or tool recursion.
            if op != "act" {
                return Err("Operator only executes snapshot-guarded act operations".into());
            }
            let observation = self.current.as_ref().ok_or("Observe before acting")?;
            if d.evidence_id.as_deref() != Some(&observation.evidence_id) {
                return Err("Stale evidence ID".into());
            }
            let snapshot = observation.payload["snapshotId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("Current observation is not actionable")?;
            if d.params["snapshotId"].as_str() != Some(snapshot) {
                return Err("Stale snapshot; observe again".into());
            }
            if let Some(image) = d.params["imageId"].as_str() {
                let known = observation.payload["images"]
                    .as_array()
                    .is_some_and(|images| images.iter().any(|i| i["imageId"] == image));
                if !known {
                    return Err("imageId does not belong to current observation".into());
                }
            }
        }
        if self.contract.channel.as_deref() == Some("jianlai") && op == "screenshot" {
            for field in ["windowId", "monitorId"] {
                if let Some(target) = self.contract.target.get(field) {
                    if d.params.get(field) != Some(target) {
                        return Err(format!("Screenshot targets a different {field}"));
                    }
                }
            }
        }
        if self.contract.channel.as_deref() == Some("chrome") {
            if let Some(tab) = self.contract.target["tabTag"].as_str() {
                if !matches!(op, "tabs" | "status") && d.params["tabTag"].as_str() != Some(tab) {
                    return Err("Operation targets a different tab".into());
                }
            }
        }
        Ok(())
    }
    pub fn observe(&mut self, evidence_id: String, payload: Value) -> Result<(), String> {
        if without_blobs(&payload).to_string().len() > MAX_OBSERVATION_BYTES {
            self.current = None;
            return Err(
                "Observation exceeds 80 KB: request a scoped inspection, never silently truncate"
                    .into(),
            );
        }
        self.last_evidence_id = Some(evidence_id.clone());
        self.current = Some(Observation {
            evidence_id,
            payload,
        });
        Ok(())
    }
    pub fn summary(&self) -> Value {
        json!({"taskId":self.id,"status":self.status,"model":self.model,"result":self.result,
            "reason":self.reason,"revision":self.revision,"verification":"agent_observed_not_backend_verified",
            "evidenceRefs":self.last_evidence_id.as_ref().map(|id|vec![id.clone()]).unwrap_or_default(),
            "metrics":{"decisions":self.decisions,"inputTextBytes":self.input_text_bytes,"inputImages":self.input_images},
            "unverifiedActions":self.actions.iter().filter(|a| matches!(a.state.as_str(),"unknown"|"dispatched"|"input_sent")).collect::<Vec<_>>()})
    }
    pub fn recover(&mut self) {
        for a in &mut self.actions {
            if a.state == "dispatched" {
                a.state = "unknown".into();
            }
        }
        if self.status == "running" {
            self.status = "needs_review".into();
            self.reason = "Interrupted task: reconcile actual UI state before new work".into();
        }
        self.current = None; // Restored evidence cannot authorize a new action.
    }
}

/// Metadata can describe images, but neither base64 nor loadable historical paths
/// belong in a text prompt/result. Images are passed separately, from current only.
pub fn without_blobs(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| {
                    !(k.as_str() == "data"
                        && (map.contains_key("imageId")
                            || map.contains_key("path")
                            || map.get("type").is_some_and(|v| v == "image")))
                        && !matches!(k.as_str(), "base64" | "imagePath" | "documentPath" | "path")
                })
                .map(|(k, v)| (k.clone(), without_blobs(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_blobs).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn task() -> Task {
        Task::new("test".into(),"owner".into(),"root".into(),ModelIdentity{agent:"codebuddy".into(),model:"same-model".into(),reasoning_effort:Some("high".into())},Request::parse(&json!({"op":"run","requestKey":"r1","goal":"Read all pages","channel":"chrome","target":{"tabTag":"t1"},"constraints":["Never submit"],"acceptance":["Read every page"]})).unwrap())
    }
    #[test]
    fn model_override_is_rejected() {
        assert!(Request::parse(&json!({"op":"run","model":"other"})).is_err());
    }
    #[test]
    fn projection_forgets_images_not_progress() {
        let mut t = task();
        t.checkpoint = json!({"pagesRead":[1,2],"unresolved":[3]});
        t.observe(
            "ev1".into(),
            json!({"snapshotId":"old","images":[{"path":"old.png","data":"BASE64_OLD"}]}),
        )
        .unwrap();
        t.observe("ev2".into(),json!({"snapshotId":"new","images":[{"imageId":"i2","path":"new.png","data":"BASE64_NEW"}]})).unwrap();
        let p = t.project(&json!({})).to_string();
        assert!(!p.contains("old"));
        assert!(!p.contains("BASE64"));
        assert!(!p.contains("new.png"));
        assert!(p.contains("Never submit"));
        assert!(p.contains("pagesRead"));
        assert!(p.contains("unresolved"));
    }
    #[test]
    fn stale_action_and_finish_rejected() {
        let mut t = task();
        t.observe("ev2".into(), json!({"snapshotId":"s2"})).unwrap();
        for kind in ["act", "finish"] {
            let d=Decision::parse(&json!({"kind":kind,"evidenceId":"ev1","params":{"operation":"act","snapshotId":"s1","tabTag":"t1"}}).to_string()).unwrap();
            assert!(t.validate_decision(&d).is_err());
        }
    }
    #[test]
    fn observation_cannot_be_a_click() {
        let t = task();
        let d = Decision::parse(r#"{"kind":"observe","params":{"operation":"act"}}"#).unwrap();
        assert!(t.validate_decision(&d).is_err());
    }
    #[test]
    fn context_budget_fails_instead_of_silent_loss() {
        let mut t = task();
        assert!(t
            .observe("e".into(), json!({"dom":"x".repeat(90_000)}))
            .is_err());
        assert!(t.current.is_none());
    }
    #[test]
    fn recovery_never_replays_dispatched_action() {
        let mut t = task();
        t.status = "running".into();
        t.actions.push(ActionRecord {
            id: "a".into(),
            operation: "act".into(),
            state: "dispatched".into(),
            evidence_id: None,
        });
        t.recover();
        assert_eq!(t.status, "needs_review");
        assert_eq!(t.actions[0].state, "unknown");
        assert!(t.current.is_none());
    }
    #[test]
    fn summary_contains_no_observation_or_image_paths() {
        let mut t = task();
        t.observe("ev".into(),json!({"snapshotId":"s","images":[{"path":"secret.png","data":"BASE64"}],"dom":"RAW_DOM"})).unwrap();
        let s = t.summary().to_string();
        assert!(!s.contains("secret.png"));
        assert!(!s.contains("RAW_DOM"));
        assert!(!s.contains("BASE64"));
    }
}
