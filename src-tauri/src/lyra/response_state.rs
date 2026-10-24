//! Private, process-local Responses continuation cache. Never stores credentials or history text.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, sync::{Arc, Mutex, OnceLock}, time::{Duration, Instant}};

/// Same session with alternating configs (e.g. thinking level) keeps separate checkpoints.
const SCOPES_PER_SESSION: usize = 4;
const CAPACITY: usize = 128 * SCOPES_PER_SESSION;
const TTL: Duration = Duration::from_secs(30 * 60);
type Fingerprint = [u8; 32];

pub(super) fn fingerprint(value: &impl Serialize) -> Fingerprint {
    // Stream serialization into the digest: image/history payloads need not be copied to hash them.
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.update(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("JSON request fingerprint");
    writer.0.finalize().into()
}

pub(super) struct Checkpoint {
    pub id: String,
    pub prefix_len: usize,
    pub prefix: Fingerprint,
}

struct Entry {
    session: Fingerprint,
    scope: Fingerprint,
    owner: Arc<()>,
    touched: Instant,
    checkpoint: Option<Checkpoint>,
    disabled: bool,
}

// ponytail: linear LRU scan under a short global lock, bounded to 128 sessions x 4 scopes;
// switch to a keyed LRU only if contention is measurable. No lock spans network IO.
fn cache() -> &'static Mutex<VecDeque<Entry>> {
    static CACHE: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub(super) fn reset(session: Option<&str>) {
    if let Some(session) = session {
        let session = fingerprint(&session);
        cache().lock().unwrap().retain(|entry| entry.session != session);
    }
}

pub(super) struct Lease {
    session: Fingerprint,
    owner: Arc<()>,
    pub checkpoint: Option<Checkpoint>,
    pub disabled: bool,
}

impl Lease {
    pub fn begin(session: &str, scope: Fingerprint) -> Self {
        let session = fingerprint(&session);
        let owner = Arc::new(());
        let mut cache = cache().lock().unwrap();
        cache.retain(|entry| entry.touched.elapsed() < TTL);
        let prior = cache.iter().position(|entry| entry.session == session && entry.scope == scope).and_then(|i| cache.remove(i));
        let (checkpoint, disabled) = prior.map_or((None, false), |entry| (entry.checkpoint, entry.disabled));
        if cache.iter().filter(|entry| entry.session == session).count() >= SCOPES_PER_SESSION {
            // Deque order is begin order: the first match is this session's least recently used scope.
            if let Some(i) = cache.iter().position(|entry| entry.session == session) { cache.remove(i); }
        }
        if cache.len() >= CAPACITY { cache.pop_front(); }
        cache.push_back(Entry { session, scope, owner: owner.clone(), touched: Instant::now(), checkpoint: None, disabled });
        Self { session, owner, checkpoint, disabled }
    }

    pub fn finish(self, checkpoint: Option<Checkpoint>, disabled: bool) {
        let mut cache = cache().lock().unwrap();
        // A dropped/cancelled future leaves no checkpoint. A concurrent newer request wins;
        // the older response must never overwrite its state, even after eviction/reinsertion.
        if let Some(entry) = cache.iter_mut().find(|entry| entry.session == self.session && Arc::ptr_eq(&entry.owner, &self.owner)) {
            entry.checkpoint = checkpoint;
            entry.disabled = disabled;
            entry.touched = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_dropped_expired_or_reset_leases_cannot_commit() {
        let session = "response-state-lease-test";
        let scope = fingerprint(&"scope");
        let first = Lease::begin(session, scope);
        let second = Lease::begin(session, scope);
        first.finish(Some(Checkpoint { id: "stale".into(), prefix_len: 0, prefix: scope }), false);
        drop(second);
        assert!(Lease::begin(session, scope).checkpoint.is_none());
        let mut cache = cache().lock().unwrap();
        // Expiry uses monotonic time, independent of wall-clock adjustment.
        if let Some(entry) = cache.iter_mut().find(|e| e.session == fingerprint(&session)) {
            entry.touched = Instant::now() - TTL;
            entry.disabled = true;
        }
        drop(cache);
        assert!(!Lease::begin(session, scope).disabled);
        let stale = Lease::begin(session, scope);
        reset(Some(session));
        stale.finish(Some(Checkpoint { id: "reset".into(), prefix_len: 0, prefix: scope }), false);
        assert!(Lease::begin(session, scope).checkpoint.is_none());
        assert!(super::cache().lock().unwrap().len() <= CAPACITY);
    }

    #[test]
    fn scopes_of_one_session_keep_separate_checkpoints_with_lru_bound() {
        let session = "response-state-scope-test";
        let commit = |scope: Fingerprint, id: &str| Lease::begin(session, scope)
            .finish(Some(Checkpoint { id: id.into(), prefix_len: 0, prefix: scope }), false);
        let id = |scope: Fingerprint| Lease::begin(session, scope).checkpoint.map(|c| c.id);
        let scopes: Vec<Fingerprint> = (0..=SCOPES_PER_SESSION).map(|i| fingerprint(&i)).collect();
        commit(scopes[0], "a");
        commit(scopes[1], "b");
        // Alternating configs no longer wipe each other's continuation.
        assert_eq!(id(scopes[0]).as_deref(), Some("a"));
        commit(scopes[0], "a");
        assert_eq!(id(scopes[1]).as_deref(), Some("b"));
        commit(scopes[1], "b");
        for (i, scope) in scopes.iter().enumerate().skip(2) { commit(*scope, &i.to_string()); }
        // The least recently begun scope (0) is evicted; the newest survive.
        assert!(id(scopes[0]).is_none());
        assert_eq!(id(scopes[SCOPES_PER_SESSION]).as_deref(), Some(SCOPES_PER_SESSION.to_string().as_str()));
        reset(Some(session));
    }
}
