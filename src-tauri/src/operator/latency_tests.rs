use super::*;
use std::sync::atomic::AtomicUsize;
fn setup(decide: Decide, reads: Arc<AtomicUsize>, fail_first: bool) -> (PathBuf, Registration) {
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
                    if fail_first && n == 0 {
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
        false,
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
        true,
    );
    let result = execute(&r.scope, &root, &request()).await.unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
