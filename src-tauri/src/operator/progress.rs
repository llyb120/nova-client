//! Bounded historical acceptance evidence and incremental checkpoints. These
//! records never authorize an input or turn an agent claim into backend proof.
use super::{Decision, Task, MAX_CHECKPOINT_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactClaim {
    pub criterion: usize,
    pub detail: String,
    pub evidence_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fact {
    claim: FactClaim,
    stale: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    #[serde(default)]
    facts: BTreeMap<usize, Fact>,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    last_target: Value,
    #[serde(default)]
    recent_checks: Vec<String>,
}
impl Progress {
    pub fn context(&self) -> Value {
        let mut v = json!({"acceptanceFacts":self.facts.values().collect::<Vec<_>>(),
            "verification":"historical_agent_observations_not_backend_verified"});
        if self
            .recent_checks
            .last()
            .is_some_and(|last| self.recent_checks.iter().filter(|v| *v == last).count() >= 3)
        {
            v["repeatWarning"] = json!("Repeated check without recorded progress. Consult acceptanceFacts/checkpoint, name the missing evidence, and avoid the same check. This warning is not proof of completion or unchanged UI.");
        }
        v
    }
    pub fn invalidate(&mut self) {
        for f in self.facts.values_mut() {
            f.stale = true;
        }
        self.recent_checks.clear();
    }
    pub fn before_action(&mut self, params: &Value) {
        // Only explicit view-only variants preserve observed facts. A key press
        // may edit a SELECT or trigger navigation and is never assumed harmless.
        let view_only = |a: &Value| {
            matches!(
                a["action"].as_str(),
                Some("scroll" | "scroll_at" | "move" | "wait")
            )
        };
        let only_view = params
            .get("actions")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty() && a.iter().all(view_only))
            .unwrap_or_else(|| params.get("action").is_some_and(view_only));
        if !only_view {
            self.invalidate();
        }
    }
    pub fn observe_scope(&mut self, payload: &Value) {
        let scope = if let Some(tab) = payload["tabTag"].as_str() {
            let urls = payload["pages"]
                .as_array()
                .map(|pages| {
                    pages
                        .iter()
                        .map(|p| json!([p["frame"], p["url"]]))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // Status-only responses carry no document identity; do not replace it.
            if urls.is_empty() {
                return;
            }
            json!([tab, urls]).to_string()
        } else if payload.get("windowId").is_some() || payload.get("monitorId").is_some() {
            json!([
                payload["windowId"],
                payload["monitorId"],
                payload["foreground"]
            ])
            .to_string()
        } else {
            return;
        };
        if !self.scope.is_empty() && self.scope != scope {
            self.invalidate();
        }
        self.scope = scope;
    }
    pub fn remember_target(&mut self, params: &Value) {
        if params["operation"] != "screenshot" {
            return;
        }
        // Retain only scope identity. Never persist crop/image/snapshot coordinates
        // here: they would be unsafe on a resume or a transient observation retry.
        let mut target = json!({});
        for field in ["windowId", "monitorId"] {
            if let Some(v) = params.get(field) {
                target[field] = v.clone();
            }
        }
        if target.as_object().is_some_and(|m| !m.is_empty()) {
            self.last_target = target;
        }
    }
}
fn merge_patch(base: &mut Value, patch: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err("Checkpoint patch nesting exceeds 16".into());
    }
    if let Some(map) = patch.as_object() {
        if !base.is_object() {
            *base = json!({});
        }
        for (key, value) in map {
            if value.is_null() {
                base.as_object_mut().unwrap().remove(key);
            } else {
                merge_patch(
                    base.as_object_mut()
                        .unwrap()
                        .entry(key.clone())
                        .or_insert(Value::Null),
                    value,
                    depth + 1,
                )?;
            }
        }
    } else {
        *base = patch.clone();
    }
    Ok(())
}
fn digest(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
impl Task {
    fn next_checkpoint(&self, d: &Decision) -> Result<Value, String> {
        if d.checkpoint.is_some() && d.checkpoint_patch.is_some() {
            return Err("Use checkpointPatch OR checkpoint, never both".into());
        }
        let mut next = d
            .checkpoint
            .clone()
            .unwrap_or_else(|| self.checkpoint.clone());
        if let Some(patch) = &d.checkpoint_patch {
            if !patch.is_object() {
                return Err("checkpointPatch must be an object".into());
            }
            merge_patch(&mut next, patch, 0)?;
        }
        if next.to_string().len() > MAX_CHECKPOINT_BYTES {
            return Err(
                "Merged checkpoint exceeds budget; split the task, never silently discard facts"
                    .into(),
            );
        }
        Ok(next)
    }
    pub fn validate_progress(&self, d: &Decision) -> Result<(), String> {
        self.next_checkpoint(d)?;
        if d.verified.len() > self.contract.acceptance.len() {
            return Err("Too many acceptance claims".into());
        }
        let mut facts = self.progress.facts.clone();
        for claim in &d.verified {
            let current = self
                .current
                .as_ref()
                .ok_or("Acceptance claim requires a current observation")?;
            if claim.criterion >= self.contract.acceptance.len()
                || claim.detail.trim().is_empty()
                || claim.detail.len() > 1200
                || claim.evidence_id != current.evidence_id
                || current.payload["snapshotId"]
                    .as_str()
                    .is_none_or(str::is_empty)
            {
                return Err("Acceptance claim requires valid criterion, bounded detail and current evidenceId".into());
            }
            facts.insert(
                claim.criterion,
                Fact {
                    claim: claim.clone(),
                    stale: false,
                },
            );
        }
        if serde_json::to_vec(&facts).map_err(|e| e.to_string())?.len() > MAX_CHECKPOINT_BYTES {
            return Err("Acceptance evidence budget exceeded; split task".into());
        }
        Ok(())
    }
    pub fn apply_progress(&mut self, d: &Decision) -> Result<(), String> {
        self.validate_progress(d)?;
        self.checkpoint = self.next_checkpoint(d)?;
        for claim in &d.verified {
            self.progress.facts.insert(
                claim.criterion,
                Fact {
                    claim: claim.clone(),
                    stale: false,
                },
            );
        }
        if matches!(d.kind.as_str(), "observe" | "act") {
            let mut params = d.params.clone();
            if let Some(m) = params.as_object_mut() {
                m.remove("snapshotId");
                m.remove("imageId");
            }
            let facts: Vec<_> = self
                .progress
                .facts
                .values()
                .map(|f| json!([f.claim.criterion, f.claim.detail, f.stale]))
                .collect();
            let key = digest(&json!([params, self.checkpoint, facts]));
            self.progress.recent_checks.push(key);
            if self.progress.recent_checks.len() > 8 {
                self.progress.recent_checks.remove(0);
            }
        }
        Ok(())
    }
    pub fn initial_observation(&self) -> Option<Decision> {
        let params = if self.contract.channel.as_deref() == Some("chrome") {
            let tab = self.contract.target["tabTag"]
                .as_str()
                .filter(|s| !s.is_empty())?;
            json!({"operation":"inspect","tabTag":tab,"scope":"viewport","visual":"auto"})
        } else {
            let mut p = json!({"operation":"screenshot"});
            let target = if self.contract.target.get("windowId").is_some()
                || self.contract.target.get("monitorId").is_some()
            {
                &self.contract.target
            } else {
                &self.progress.last_target
            };
            let mut bound = false;
            for field in ["windowId", "monitorId"] {
                if let Some(v) = target.get(field) {
                    p[field] = v.clone();
                    bound = true;
                }
            }
            if !bound {
                return None;
            }
            p
        };
        Decision::parse(&json!({"kind":"observe","params":params}).to_string()).ok()
    }
    pub fn observation_capabilities(&self) -> Value {
        let Some(o) = &self.current else {
            return json!({"actionable":false});
        };
        let images = o.payload["images"]
            .as_array()
            .is_some_and(|i| !i.is_empty());
        let actionable = o.payload["snapshotId"]
            .as_str()
            .is_some_and(|s| !s.is_empty());
        json!({"actionable":actionable,"hasCurrentImage":images,
            "drag":actionable && images && (self.contract.channel.as_deref() != Some("chrome") || o.payload["fullPage"] == false),
            "dragRequirement":"Chrome: fresh fullPage=false viewport image. Never reuse coordinates after refreshing."})
    }
    pub fn validate_capabilities(&self, d: &Decision) -> Result<(), String> {
        let drag = |a: &Value| a["action"] == "drag";
        let has_drag = d
            .params
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(drag))
            || d.params.get("action").is_some_and(drag);
        if has_drag
            && self.contract.channel.as_deref() == Some("chrome")
            && self
                .current
                .as_ref()
                .is_some_and(|o| o.payload["fullPage"] != false)
        {
            return Err("Drag requires a current fullPage=false viewport screenshot; refresh then decide new coordinates".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task() -> Task {
        super::super::tests::task()
    }
    fn decision(v: Value) -> Decision {
        Decision::parse(&v.to_string()).unwrap()
    }
    #[test]
    fn patch_preserves_unmentioned_facts_and_replaces_arrays() {
        let mut t = task();
        t.checkpoint = json!({"pages":[1],"data":{"a":1,"b":2},"remove":true});
        t.apply_progress(&decision(json!({"kind":"observe","checkpointPatch":{"pages":[1,2],"data":{"b":3},"remove":null}}))).unwrap();
        assert_eq!(t.checkpoint, json!({"pages":[1,2],"data":{"a":1,"b":3}}));
        t.apply_progress(&decision(json!({"kind":"observe"})))
            .unwrap();
        assert_eq!(t.checkpoint["data"]["a"], 1);
    }
    #[test]
    fn merged_budget_is_checked_without_partial_mutation() {
        let mut t = task();
        t.checkpoint = json!({"a":"x".repeat(7000)});
        let before = t.checkpoint.clone();
        assert!(t
            .apply_progress(&decision(
                json!({"kind":"observe","checkpointPatch":{"b":"x".repeat(7000)}})
            ))
            .is_err());
        assert_eq!(t.checkpoint, before);
        assert!(t
            .apply_progress(&decision(
                json!({"kind":"observe","checkpoint":{},"checkpointPatch":{}})
            ))
            .is_err());
        assert!(t
            .apply_progress(&decision(json!({"kind":"observe","checkpointPatch":[]})))
            .is_err());
    }
    #[test]
    fn claims_are_current_bounded_and_not_backend_verified() {
        let mut t = task();
        t.observe("e1".into(), json!({"snapshotId":"s1"})).unwrap();
        let mut d = decision(
            json!({"kind":"observe","verified":[{"criterion":0,"detail":"page read","evidenceId":"old"}]}),
        );
        assert!(t.apply_progress(&d).is_err());
        d.verified[0].evidence_id = "e1".into();
        t.apply_progress(&d).unwrap();
        assert_eq!(t.progress.context()["acceptanceFacts"][0]["stale"], false);
        assert_eq!(
            t.summary()["verification"],
            "agent_observed_not_backend_verified"
        );
        d.verified[0].criterion = 100;
        assert!(t.apply_progress(&d).is_err());
    }
    #[test]
    fn scroll_preserves_historical_fact_but_edit_and_resume_invalidate() {
        let mut t = task();
        t.observe("e1".into(), json!({"snapshotId":"s1"})).unwrap();
        let d = decision(
            json!({"kind":"observe","verified":[{"criterion":0,"detail":"read","evidenceId":"e1"}]}),
        );
        t.apply_progress(&d).unwrap();
        t.progress
            .before_action(&json!({"actions":[{"action":"scroll"}]}));
        assert!(!t.progress.facts[&0].stale);
        t.progress
            .before_action(&json!({"actions":[{"action":"press","key":"Tab"}]}));
        assert!(t.progress.facts[&0].stale);
        t.apply_progress(&d).unwrap();
        t.recover();
        assert!(t.progress.facts[&0].stale);
        assert!(t.current.is_none());
    }
    #[test]
    fn changed_scope_invalidates_facts() {
        let mut t = task();
        t.observe(
            "e1".into(),
            json!({"snapshotId":"s1","tabTag":"t1","pages":[{"frame":0,"url":"page-a"}]}),
        )
        .unwrap();
        t.apply_progress(&decision(json!({"kind":"observe","verified":[{"criterion":0,"detail":"read","evidenceId":"e1"}]}))).unwrap();
        t.observe(
            "e2".into(),
            json!({"snapshotId":"s2","tabTag":"t1","pages":[{"frame":0,"url":"page-b"}]}),
        )
        .unwrap();
        assert!(t.progress.facts[&0].stale);
    }
    #[test]
    fn full_page_drag_is_rejected_before_native_dispatch() {
        let mut t = task();
        t.observe(
            "e1".into(),
            json!({"snapshotId":"s1","fullPage":true,"images":[{"imageId":"i1"}]}),
        )
        .unwrap();
        let d = decision(
            json!({"kind":"act","evidenceId":"e1","params":{"operation":"act","tabTag":"t1","snapshotId":"s1","imageId":"i1","action":{"action":"drag","x":1,"y":1,"to_x":2,"to_y":2}}}),
        );
        assert!(t
            .validate_decision(&d)
            .unwrap_err()
            .contains("fullPage=false"));
        t.current.as_mut().unwrap().payload["fullPage"] = json!(false);
        assert!(t.validate_decision(&d).is_ok());
    }
    #[test]
    fn bootstrap_never_guesses_target_or_reuses_crop_coordinates() {
        let mut t = task();
        assert_eq!(t.initial_observation().unwrap().params["tabTag"], "t1");
        t.contract.channel = Some("jianlai".into());
        t.contract.target = json!({});
        assert!(t.initial_observation().is_none());
        t.progress.remember_target(&json!({"operation":"screenshot","windowId":42,"snapshotId":"old","imageId":"old","region":{"x":999}}));
        let d = t.initial_observation().unwrap();
        assert_eq!(d.params, json!({"operation":"screenshot","windowId":42}));
    }
    #[test]
    fn repeated_checks_warn_without_claiming_completion_or_skipping_input() {
        let mut t = task();
        let d = decision(json!({"kind":"observe","params":{"operation":"inspect","tabTag":"t1"}}));
        for _ in 0..3 {
            t.apply_progress(&d).unwrap();
        }
        assert!(t.progress.context().get("repeatWarning").is_some());
        assert_eq!(t.status, "yielded");
        t.apply_progress(&decision(
            json!({"kind":"observe","params":d.params,"checkpointPatch":{"pages":[1]}}),
        ))
        .unwrap();
        assert!(t.progress.context().get("repeatWarning").is_none());
    }
    #[test]
    fn older_persisted_tasks_default_to_empty_progress() {
        let t = task();
        let mut raw = serde_json::to_value(t).unwrap();
        raw.as_object_mut().unwrap().remove("progress");
        let restored: Task = serde_json::from_value(raw).unwrap();
        assert!(restored.progress.facts.is_empty());
    }
}
