use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use magpie_core::catalog::Catalog;

#[path = "http.rs"]
mod http;

/// Bounds concurrent work and per-request buffering for the local Gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayLimits {
    /// Maximum number of connections handled at once.
    pub active_requests: usize,
    /// Maximum number of accepted connections waiting for a worker.
    pub waiting_requests: usize,
    /// Maximum accepted request body size.
    pub request_body_bytes: usize,
    /// Maximum aggregate request buffering reserved by the service.
    pub total_buffered_bytes: usize,
    /// Maximum chunk size used by future streaming request paths.
    pub stream_chunk_bytes: usize,
}

impl Default for GatewayLimits {
    fn default() -> Self {
        Self {
            active_requests: 4,
            waiting_requests: 8,
            request_body_bytes: 1024 * 1024,
            total_buffered_bytes: 4 * 1024 * 1024,
            stream_chunk_bytes: 16 * 1024,
        }
    }
}

/// A bound local Gateway listener ready to serve connections.
#[derive(Debug)]
pub struct GatewayServer {
    listener: TcpListener,
    limits: GatewayLimits,
    models_json: Arc<str>,
}

impl GatewayServer {
    /// Binds the Gateway to the default loopback address.
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be bound.
    pub fn bind() -> io::Result<Self> {
        Self::bind_with_limits(
            magpie_core::gateway::default_gateway_addr(),
            GatewayLimits::default(),
        )
    }

    /// Binds a loopback listener using explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is not loopback or cannot be bound.
    pub fn bind_with_limits(addr: SocketAddr, limits: GatewayLimits) -> io::Result<Self> {
        Self::bind_with_catalog(addr, limits, Catalog::default())
    }

    /// Binds a loopback listener serving an immutable configured model catalog.
    ///
    /// The model list is prepared once and shared among request workers.
    ///
    /// # Errors
    ///
    /// Returns an error if the address is not loopback or cannot be bound.
    pub fn bind_with_catalog(
        addr: SocketAddr,
        limits: GatewayLimits,
        catalog: Catalog,
    ) -> io::Result<Self> {
        if !addr.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gateway only binds to loopback addresses",
            ));
        }
        let listener = TcpListener::bind(addr)?;
        Ok(Self {
            listener,
            limits,
            models_json: Arc::from(catalog.models_json()),
        })
    }

    /// Returns the address selected by the operating system.
    ///
    /// This is useful when binding to port zero in an embedding application.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves bounded HTTP requests until the listener fails.
    ///
    /// The listener is consumed so its worker pool has one clear owner.
    ///
    /// # Errors
    ///
    /// Returns an error if a worker cannot start or accepting connections fails.
    pub fn run(self) -> io::Result<()> {
        let worker_count = self.limits.active_requests.clamp(1, 16);
        let queue_size = self.limits.waiting_requests.min(128);
        let (sender, receiver) = mpsc::sync_channel::<TcpStream>(queue_size);
        let receiver = Arc::new(Mutex::new(receiver));

        for index in 0..worker_count {
            let receiver = Arc::clone(&receiver);
            let limits = self.limits;
            let models_json = Arc::clone(&self.models_json);
            thread::Builder::new()
                .name(format!("magpie-gateway-{index}"))
                .stack_size(512 * 1024)
                .spawn(move || {
                    loop {
                        let stream = {
                            let receiver = receiver
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            receiver.recv()
                        };
                        let Ok(stream) = stream else {
                            break;
                        };
                        http::handle_connection(stream, limits, &models_json);
                    }
                })?;
        }

        loop {
            match self.listener.accept() {
                Ok((stream, _)) => match sender.try_send(stream) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(mut stream)) => {
                        http::write_overloaded(&mut stream);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Gateway workers stopped",
                        ));
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}
