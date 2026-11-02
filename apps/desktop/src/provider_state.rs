//! Presentation state, never a second persistent Provider store.
use crate::provider_bridge::{Failure, Request, Snapshot};

#[derive(Default)]
pub(crate) struct ProviderState {
    pub snapshot: Option<Snapshot>,
    pub error: Option<Failure>,
    pub stale: bool,
    pub uncertain: bool,
    pub busy: Option<&'static str>,
}
impl ProviderState {
    pub fn can_write(&self) -> bool {
        self.snapshot.is_some() && self.busy.is_none() && !self.stale && !self.uncertain
    }
    pub fn begin(&mut self, request: &Request) -> bool {
        if self.busy.is_some()
            || (request.command != "list" && request.command != "recover" && !self.can_write())
        {
            return false;
        }
        self.busy = Some(request.command);
        self.error = None;
        true
    }
    pub fn finish(&mut self, _request: &Request, result: Result<Snapshot, Failure>) -> bool {
        self.busy = None;
        match result {
            Ok(snapshot) => {
                let ok = snapshot
                    .operation
                    .as_ref()
                    .is_none_or(|operation| operation.ok);
                self.error = snapshot
                    .operation
                    .as_ref()
                    .filter(|o| !o.ok)
                    .map(|o| Failure {
                        code: o.code.clone(),
                        uncertain: false,
                    });
                self.snapshot = Some(snapshot);
                self.stale = false;
                self.uncertain = false;
                ok
            }
            Err(failure) => {
                self.uncertain |= failure.uncertain;
                // A failed read-only test can also report a revision conflict
                // or unreadable store. Preserve the view, but require a fresh
                // snapshot after every command failure before further writes.
                self.stale = true;
                self.error = Some(failure);
                false
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(revision: u64) -> Snapshot {
        Snapshot {
            version: "0.2.0".into(),
            schema_version: 1,
            revision,
            token: format!("{revision}:{}", "0".repeat(64)),
            providers: Vec::new(),
            operation: None,
        }
    }
    #[test]
    fn conflict_preserves_view_and_requires_reread() {
        let mut s = ProviderState::default();
        s.finish(&Request::list(), Ok(snapshot(1)));
        let r = Request {
            command: "edit",
            id: Some("demo".into()),
            token: Some("token".into()),
            data: None,
        };
        assert!(s.begin(&r));
        s.finish(
            &r,
            Err(Failure {
                code: "provider_conflict".into(),
                uncertain: false,
            }),
        );
        assert_eq!(s.snapshot.as_ref().unwrap().revision, 1);
        assert!(!s.can_write());
        assert!(s.begin(&Request::list()));
        s.finish(&Request::list(), Ok(snapshot(2)));
        assert!(s.can_write());
    }
    #[test]
    fn cancelled_write_never_claims_rollback() {
        let mut s = ProviderState::default();
        s.finish(&Request::list(), Ok(snapshot(1)));
        let r = Request {
            command: "add",
            id: Some("demo".into()),
            token: None,
            data: None,
        };
        s.begin(&r);
        s.finish(
            &r,
            Err(Failure {
                code: "operation_cancelled".into(),
                uncertain: true,
            }),
        );
        assert!(s.uncertain);
        assert!(!s.can_write());
    }
    #[test]
    fn one_job_at_a_time() {
        let mut s = ProviderState::default();
        assert!(s.begin(&Request::list()));
        assert!(!s.begin(&Request::list()));
    }
    #[test]
    fn failed_readonly_test_cannot_keep_old_snapshot_fresh() {
        let mut s = ProviderState::default();
        s.finish(&Request::list(), Ok(snapshot(1)));
        let request = Request {
            command: "test",
            id: Some("demo".into()),
            token: None,
            data: None,
        };
        assert!(s.begin(&request));
        s.finish(
            &request,
            Err(Failure {
                code: "provider_revision_conflict".into(),
                uncertain: false,
            }),
        );
        assert!(s.stale);
        assert!(!s.uncertain);
        assert!(!s.can_write());
        assert_eq!(s.snapshot.unwrap().revision, 1);
    }
}
