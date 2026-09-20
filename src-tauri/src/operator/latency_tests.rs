use super::*;
use std::sync::atomic::AtomicUsize;
fn setup(
    decide: Decide,
    reads: Arc<AtomicUsize>,
    fail_at: Option<usize>,
) -> (PathBuf, Registration) {
    let root = std::env::temp_dir().join(format!("operator-latency-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let r = register(
        &Uuid::new_v4().to_string(),
        root.clone(),
        root.clone(),
        ModelIdentity {
            agent: "fixture".into(),
            model: "inherited".into(),
            reasoning_effort: None,
        },
        decide,
        Native {
            chrome: json!({}),
            jianlai: json!({}),
            execute: Arc::new(move |_, _, p, _| {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    assert_ne!(p["operation"], "act");
                    if fail_at == Some(n) {
                        return Err("viewport changed during capture".into());
                    }
                    Ok(json!({"snapshotId":"fresh","tabTag":"t1","dom":"expected"}))
                })
            }),
        },
    );
    (root, r)
}
fn request() -> Value {
    json!({"op":"run","requestKey":"once","channel":"chrome","target":{"tabTag":"t1"},"goal":"Read","acceptance":["Read"]})
}
#[tokio::test]
async fn bound_target_is_observed_without_a_model_roundtrip() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model_calls = calls.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let (root, r) = setup(
        Arc::new(move |i| {
            model_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                assert_eq!(
                    i.context["currentObservation"]["data"]["snapshotId"],
                    "fresh"
                );
                Ok(json!({"kind":"finish","evidenceId":i.context["currentObservation"]["evidenceId"],"result":{"read":true}}).to_string())
            })
        }),
        reads.clone(),
        None,
    );
    let result = execute(&r.scope, &root, &request()).await.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn read_only_capture_retry_does_not_ask_model_to_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model_calls = calls.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let (root, r) = setup(
        Arc::new(move |i| {
            model_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(json!({"kind":"finish","evidenceId":i.context["currentObservation"]["evidenceId"],"result":{}}).to_string())
            })
        }),
        reads.clone(),
        Some(0),
    );
    let result = execute(&r.scope, &root, &request()).await.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn read_retry_drops_already_applied_evidence_claims() {
    let calls = Arc::new(AtomicUsize::new(0));
    let model_calls = calls.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let (root, r) = setup(
        Arc::new(move |i| {
            let n = model_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let evidence = i.context["currentObservation"]["evidenceId"].clone();
                if n == 0 {
                    Ok(json!({"kind":"observe", "params":{"operation":"inspect", "tabTag":"t1"},
                        "checkpointPatch":{"saved":"keep"},
                        "verified":[{"criterion":0,"detail":"read before recapture","evidenceId":evidence}]
                    }).to_string())
                } else {
                    assert_eq!(i.context["checkpoint"]["saved"], "keep");
                    assert_eq!(i.context["progress"]["acceptanceFacts"][0]["stale"], true);
                    Ok(json!({"kind":"finish","evidenceId":evidence,"result":{}}).to_string())
                }
            })
        }),
        reads.clone(),
        Some(1),
    );
    let result = execute(&r.scope, &root, &request()).await.unwrap();
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
