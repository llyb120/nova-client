"""Apply the reviewed Operator-only fix, refusing a different source baseline.
No native input implementation, user setting or Reasonix source is changed.
"""
from pathlib import Path
import hashlib
p=Path('src-tauri/src/operator/runtime.rs')
s=p.read_text()
assert hashlib.sha1(b'blob '+str(len(s.encode())).encode()+b'\0'+s.encode()).hexdigest()=='3122f3448fe2870372c71760b152e77d9b743cb6','Unexpected runtime baseline'
def replace(old,new):
 global s
 assert s.count(old)==1,old[:90]
 s=s.replace(old,new)
replace('A tool schema in the context describes operations for params, not an instruction to invoke another agent tool.";', 'A tool schema in the context describes operations for params, not an instruction to invoke another agent tool. Wire format: evidenceId is a TOP-LEVEL sibling of kind and params; it is NOT a native tool parameter. Copy currentObservation.evidenceId to top-level evidenceId, and currentObservation.data.snapshotId to params.snapshotId. Never place evidenceId inside params. Action objects must contain only fields defined for that action variant; for example press has key, not frame/ref/text. Only observe operations listed by this runtime are permitted; ignore unrelated catalog advice about experience tools. If lastDecisionError exists, the rejected decision sent NO new input: correct the envelope or request a fresh observation, never replay an earlier dispatched action.";')
replace('    for _ in 0..6 {\n', '    // Retrying a rejected decision is safe only BEFORE dispatch. Native partial\n    // execution and unknown outcomes keep their existing stop-for-review path.\n    let mut last_decision_error: Option<String> = None;\n    let mut rejected_decisions = 0u32;\n    for _ in 0..6 {\n')
replace('        let (context, current) = {','        let (mut context, current) = {')
replace('        let images = load_images(current.as_ref().map(|o| &o.payload)).await?;', '''        if let Some(error) = last_decision_error.take() {
            context["lastDecisionError"] = json!({"error":error,"inputDispatched":false,
                "instruction":"Generate a NEW valid decision. Do not repeat any earlier native action. evidenceId belongs at top level, snapshotId inside params."});
        }
        let images = load_images(current.as_ref().map(|o| &o.payload)).await?;''')
replace('        let d = Decision::parse(&text)?;', '''        let checked = Decision::parse(&text).and_then(|d| {
            live.task.lock().unwrap().validate_decision(&d)?;
            Ok(d)
        });
        let d = match checked {
            Ok(d) => d,
            Err(error) => {
                let unknown = live.task.lock().unwrap().actions.iter()
                    .any(|a| matches!(a.state.as_str(), "unknown" | "dispatched"));
                if unknown || rejected_decisions >= 2 { return Err(error); }
                rejected_decisions += 1;
                last_decision_error = Some(error);
                continue;
            }
        };''')
anchor='    #[tokio::test]\n    async fn isolated_task_completes_and_duplicate_run_does_not_execute() {'
tests='''    #[tokio::test]
    async fn malformed_envelope_is_redecided_without_native_action() {
        let decide: Decide = Arc::new(|input| Box::pin(async move {
            let c = &input.context["currentObservation"];
            Ok(if c.is_null() {
                json!({"kind":"observe","params":{"operation":"inspect"}})
            } else if input.context.get("lastDecisionError").is_none() {
                json!({"kind":"act","params":{"operation":"act","snapshotId":"snapshot-current","evidenceId":c["evidenceId"],"action":{"action":"press","key":"Enter"}}})
            } else {
                assert_eq!(input.context["lastDecisionError"]["inputDispatched"], false);
                json!({"kind":"finish","evidenceId":c["evidenceId"],"result":{"reviewed":true}})
            }.to_string())
        }));
        let (root, r, actions) = fixture(decide);
        let result = execute(&r.scope, &root, &request()).await.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(actions.load(Ordering::SeqCst), 0);
        assert_eq!(result["metrics"]["decisions"], 3);
    }
    #[tokio::test]
    async fn repeated_invalid_decisions_are_bounded_and_never_dispatched() {
        let decide: Decide = Arc::new(|_| Box::pin(async { Ok("not JSON".into()) }));
        let (root, r, actions) = fixture(decide);
        let result = execute(&r.scope, &root, &request()).await.unwrap();
        assert_eq!(result["status"], "blocked");
        assert_eq!(actions.load(Ordering::SeqCst), 0);
        assert_eq!(result["metrics"]["decisions"], 3);
    }
'''
replace(anchor,tests+anchor)
p.write_text(s)
print('Applied Operator-only protocol clarification and bounded pre-dispatch retry. No actions are rewritten or replayed.')
