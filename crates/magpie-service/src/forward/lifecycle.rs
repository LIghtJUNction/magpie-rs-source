//! A desktop owns a child through a private pipe, never through a listening port.

use super::*;

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// Public diagnostics, not an authentication token or proof of ownership.
#[derive(Debug, Clone)]
pub struct GatewayIdentity {
    pub server_version: &'static str,
    pub address: SocketAddr,
    pub instance_id: String,
    /// The kind of source, not a claim about which file an external server uses.
    pub config_source: &'static str,
}

impl GatewayIdentity {
    pub(super) fn new(address: SocketAddr, config_source: &'static str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            server_version: magpie_core::VERSION,
            address,
            instance_id: format!(
                "{:x}-{timestamp:x}-{:x}",
                std::process::id(),
                NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
            ),
            config_source,
        }
    }

    /// The sole stdout event in supervised CLI mode. No paths or keys are sent.
    pub fn ready_json(&self) -> String {
        json!({
            "event":"gateway_ready", "protocol_version":1,
            "server_version":self.server_version,
            "address":self.address.to_string(), "instance_id":self.instance_id,
            "config_source":self.config_source,
        })
        .to_string()
    }
}

impl ForwardServer {
    /// Runs independently until the process is stopped or the listener fails.
    pub fn run(self) -> io::Result<()> {
        self.run_controlled(std::future::pending(), |_| Ok(()))
    }

    /// Runs while an owner's private input pipe remains open. Closing that pipe
    /// (also when the owner crashes) revokes the listener and cancels requests.
    /// This is intended for a dedicated CLI child; the blocking lease reader
    /// lives until EOF, and must not be given an unrelated process's stdin.
    pub fn run_supervised<R, F>(self, mut owner: R, ready: F) -> io::Result<()>
    where
        R: Read + Send + 'static,
        F: FnOnce(&GatewayIdentity) -> io::Result<()>,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("magpie-owner-lease".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut buffer = [0; 64];
                loop {
                    match owner.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = sender.send(());
            })?;
        self.run_controlled(
            async {
                let _ = receiver.await;
            },
            ready,
        )
    }

    fn run_controlled<S, F>(self, shutdown: S, ready: F) -> io::Result<()>
    where
        S: std::future::Future<Output = ()>,
        F: FnOnce(&GatewayIdentity) -> io::Result<()>,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(self.run_async(shutdown, ready));
        // Native credential calls cannot be forcibly cancelled. Request tasks
        // and the listener are cancelled first; a child never hangs on exit.
        runtime.shutdown_timeout(Duration::from_millis(200));
        result
    }

    async fn run_async<S, F>(self, shutdown: S, ready: F) -> io::Result<()>
    where
        S: std::future::Future<Output = ()>,
        F: FnOnce(&GatewayIdentity) -> io::Result<()>,
    {
        let address = self.listener.local_addr()?;
        let source = self.source.initialize(address)?;
        let client = source::transport_client()?;
        let requests = Arc::new(Semaphore::new(ACTIVE_LIMIT));
        let connections = Arc::new(Semaphore::new(CONNECTION_LIMIT));
        let state = Arc::new(State {
            source,
            address,
            client,
            requests,
            identity: self.identity,
            catalogs: Arc::new(Semaphore::new(CATALOG_ACTIVE_LIMIT)),
            stats: Arc::new(Stats::default()),
        });
        let listener = tokio::net::TcpListener::from_std(self.listener)?;
        // All service initialization has succeeded; a bind-only event is not ready.
        ready(&state.identity)?;
        let accept = async move {
            loop {
                let permit = connections
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| invalid("listener_closed"))?;
                let (socket, peer) = listener.accept().await?;
                if !peer.ip().is_loopback() {
                    continue;
                }
                let state = state.clone();
                tokio::spawn(async move {
                    let _connection_permit = permit;
                    let service = service_fn(move |request| handle(request, state.clone()));
                    let _ = timeout(
                        TOTAL_TIMEOUT + Duration::from_secs(5),
                        serve_forward_connection(TokioIo::new(socket), service),
                    )
                    .await;
                });
            }
            #[allow(unreachable_code)]
            Ok::<(), io::Error>(())
        };
        tokio::select! {
            biased;
            () = shutdown => Ok(()),
            result = accept => result,
        }
    }
}
