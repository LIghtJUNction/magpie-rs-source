//! A gateway owned by this window. An external listener is never adopted or killed.
use async_process::{Child, Command};
use futures_lite::{AsyncBufReadExt, AsyncReadExt, io::BufReader};
use std::{net::SocketAddr, path::Path, process::Stdio, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Address,
    Cli,
    Startup,
    Timeout,
    Exited,
    Cleanup,
}

pub(crate) struct GatewayProcess {
    child: Child,
    pub(crate) address: SocketAddr,
}

impl GatewayProcess {
    pub(crate) async fn start(address: &str, store: &Path) -> Result<Self, Error> {
        let address = parse_address(address)?;
        let command = crate::cli_bridge::gateway_command()
            .await
            .map_err(|_| Error::Cli)?;
        Self::spawn(command, address, store).await
    }

    async fn spawn(
        mut command: std::process::Command,
        address: SocketAddr,
        store: &Path,
    ) -> Result<Self, Error> {
        command
            .args(["serve", "--listen", &address.to_string(), "--providers"])
            .arg(store);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = Command::from(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .reap_on_drop(true)
            .spawn()
            .map_err(|_| Error::Startup)?;
        let output = child.stdout.take().ok_or(Error::Startup)?;
        let startup = async {
            let mut line = String::new();
            BufReader::new(output.take(1024))
                .read_line(&mut line)
                .await
                .map_err(|_| Error::Startup)?;
            // The CLI emits this only after bind succeeds. A failed bind cannot
            // be mistaken for a pre-existing gateway answering on that port.
            let bound: SocketAddr = line
                .split_once("bound at ")
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .and_then(|value| value.parse().ok())
                .ok_or(Error::Startup)?;
            if bound.ip() != address.ip() || (address.port() != 0 && address.port() != bound.port())
            {
                return Err(Error::Startup);
            }
            Ok(bound)
        };
        let timeout = async {
            async_io::Timer::after(Duration::from_secs(5)).await;
            Err(Error::Timeout)
        };
        let result = futures_lite::future::race(startup, timeout).await;
        match result {
            Ok(address) if child.try_status().map_err(|_| Error::Startup)?.is_none() => {
                Ok(Self { child, address })
            }
            result => {
                let _ = child.kill();
                let _ = child.status().await;
                Err(result.err().unwrap_or(Error::Startup))
            }
        }
    }

    pub(crate) fn is_running(&mut self) -> bool {
        matches!(self.child.try_status(), Ok(None))
    }

    pub(crate) async fn stop(mut self) -> Result<(), Error> {
        if self
            .child
            .try_status()
            .map_err(|_| Error::Cleanup)?
            .is_none()
        {
            self.child.kill().map_err(|_| Error::Cleanup)?;
        }
        self.child
            .status()
            .await
            .map(|_| ())
            .map_err(|_| Error::Cleanup)
    }
}

fn parse_address(address: &str) -> Result<SocketAddr, Error> {
    address
        .parse::<SocketAddr>()
        .ok()
        .filter(|address| address.ip().is_loopback())
        .ok_or(Error::Address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{TcpListener, TcpStream},
        time::SystemTime,
    };

    #[test]
    fn reject_public_listeners() {
        assert_eq!(parse_address("0.0.0.0:3425"), Err(Error::Address));
        assert_eq!(parse_address("[::]:3425"), Err(Error::Address));
        assert!(parse_address("127.0.0.1:0").is_ok());
        assert!(parse_address("[::1]:3425").is_ok());
    }

    #[test]
    #[ignore = "requires MAGPIE_GATEWAY_TEST_CLI pointing to a built CLI"]
    fn real_gateway_ownership_and_cleanup() {
        futures_lite::future::block_on(async {
            let cli = std::env::var_os("MAGPIE_GATEWAY_TEST_CLI").expect("test CLI");
            let root = std::env::temp_dir().join(format!(
                "magpie-owned-gateway-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let store = root.join("providers.toml");
            let command = || std::process::Command::new(&cli);
            let mut gateway =
                GatewayProcess::spawn(command(), "127.0.0.1:0".parse().unwrap(), &store)
                    .await
                    .expect("start");
            let address = gateway.address;
            assert!(gateway.is_running());
            assert!(TcpStream::connect(address).is_ok());
            gateway.stop().await.expect("stop and reap");
            assert!(TcpListener::bind(address).is_ok(), "owned port released");

            let external = TcpListener::bind("127.0.0.1:0").unwrap();
            let external_address = external.local_addr().unwrap();
            assert!(
                GatewayProcess::spawn(command(), external_address, &store)
                    .await
                    .is_err(),
                "occupied port fails"
            );
            assert!(
                TcpStream::connect(external_address).is_ok(),
                "external listener survives"
            );

            let gateway = GatewayProcess::spawn(command(), "127.0.0.1:0".parse().unwrap(), &store)
                .await
                .unwrap();
            let address = gateway.address;
            drop(gateway);
            for _ in 0..100 {
                if TcpListener::bind(address).is_ok() {
                    break;
                }
                async_io::Timer::after(Duration::from_millis(10)).await;
            }
            assert!(
                TcpListener::bind(address).is_ok(),
                "window drop kills its own gateway"
            );
            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
