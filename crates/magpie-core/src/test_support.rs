//! Hidden generic transaction failpoints and fork/descriptor coordination for synthetic tests.
//! Not enabled in production builds unless the explicit test-support feature is selected.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub const CRASH_EXIT: i32 = 86;

// CLOEXEC closes inherited descriptors at exec, not fork. Keep test-worker
// spawning out of parent descriptor lifetimes without serializing
// independent transactions or the lifetime of a paused worker.
static FORK_GATE: RwLock<()> = RwLock::new(());

struct DescriptorScopes {
    depth: usize,
    guard: Option<RwLockReadGuard<'static, ()>>,
}

thread_local! {
    static DESCRIPTOR_SCOPES: RefCell<DescriptorScopes> = const {
        RefCell::new(DescriptorScopes { depth: 0, guard: None })
    };
}

/// Prevent unrelated test workers from inheriting this thread's lock FDs.
#[derive(Debug)]
pub struct DescriptorScope(PhantomData<Rc<()>>);

/// Nestable on one thread; operations on different threads share the gate.
pub fn descriptor_scope() -> DescriptorScope {
    DESCRIPTOR_SCOPES.with(|scopes| {
        let mut scopes = scopes.borrow_mut();
        if scopes.depth == 0 {
            scopes.guard = Some(FORK_GATE.read().expect("test fork gate poisoned"));
        }
        scopes.depth += 1;
    });
    DescriptorScope(PhantomData)
}

impl Drop for DescriptorScope {
    fn drop(&mut self) {
        DESCRIPTOR_SCOPES.with(|scopes| {
            let mut scopes = scopes.borrow_mut();
            scopes.depth -= 1;
            if scopes.depth == 0 {
                scopes.guard = None;
            }
        });
    }
}

/// Hold only through Command::spawn, releasing before waiting on the worker.
pub fn spawn_scope() -> RwLockWriteGuard<'static, ()> {
    DESCRIPTOR_SCOPES.with(|scopes| {
        assert_eq!(
            scopes.borrow().depth,
            0,
            "a test must close its transaction descriptors before spawning a worker"
        );
    });
    FORK_GATE.write().expect("test fork gate poisoned")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Point {
    LockCreated,
    LockAcquired,
    LegacyLockLinked,
    StageSynced,
    ImageSynced,
    BeforeInstall,
    BeforeDirectorySync,
}

pub enum Action {
    Fail,
    Exit,
    /// Notify the parent, which kills this process without running Drop.
    Pause,
}

struct Fault {
    point: Point,
    path: PathBuf,
    remaining: usize,
    action: Action,
}

thread_local! {
    static FAULT: RefCell<Option<Fault>> = const { RefCell::new(None) };
}

pub fn arm(point: Point, path: &Path, occurrence: usize, action: Action) {
    assert!(occurrence > 0, "fault occurrence must be nonzero");
    FAULT.with(|fault| {
        *fault.borrow_mut() = Some(Fault {
            point,
            path: path.to_path_buf(),
            remaining: occurrence,
            action,
        });
    });
}

pub fn checkpoint(point: Point, path: &Path) -> std::io::Result<()> {
    let action = FAULT.with(|fault| {
        let mut fault = fault.borrow_mut();
        let armed = fault.as_mut()?;
        if armed.point != point || armed.path != path {
            return None;
        }
        armed.remaining -= 1;
        if armed.remaining == 0 {
            fault.take().map(|fault| fault.action)
        } else {
            None
        }
    });
    match action {
        Some(Action::Fail) => Err(std::io::Error::other("injected_transaction_failure")),
        Some(Action::Exit) => std::process::exit(CRASH_EXIT),
        Some(Action::Pause) => {
            use std::io::Write;
            println!("magpie-transaction-checkpoint");
            std::io::stdout().flush().expect("flush fixture checkpoint");
            loop {
                std::thread::park();
            }
        }
        None => Ok(()),
    }
}
