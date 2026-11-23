//! A SOCKS5 relay on loopback for client libraries that open connections by
//! themselves, such as a Kafka client dialing the brokers a cluster advertises.
//! The relay resolves each requested host, keeps the addresses the egress
//! policy permits and connects to those, so every connection the library opens
//! passes the guard.

use crate::http_client::{connect_pinned_tcp, resolve_addrs_where, HostGuardError};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

/// Which resolved addresses a relayed connection may reach.
pub(crate) type Permit = Arc<dyn Fn(&SocketAddr) -> bool + Send + Sync>;

/// Time a client has to finish the SOCKS5 request and the relay has to connect.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connections relayed at once; a connection past it is closed at accept.
const MAX_RELAYED: usize = 1024;

// SOCKS5 (RFC 1928).
const VERSION: u8 = 5;
const NO_AUTH: u8 = 0;
const NO_ACCEPTABLE_METHOD: u8 = 0xFF;
const CONNECT: u8 = 1;
const ATYP_V4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_V6: u8 = 4;
const REPLY_OK: u8 = 0;
const REPLY_NOT_ALLOWED: u8 = 2;
const REPLY_HOST_UNREACHABLE: u8 = 4;
const REPLY_REFUSED: u8 = 5;
const REPLY_COMMAND_NOT_SUPPORTED: u8 = 7;
const REPLY_ADDRESS_NOT_SUPPORTED: u8 = 8;

/// Listen on an ephemeral loopback port and relay its connections on the
/// current Tokio runtime, for as long as the runtime runs. Returns the address
/// to give the client library as its SOCKS5 proxy.
pub(crate) fn start(permit: Permit) -> io::Result<SocketAddr> {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(listener)?;
    let addr = listener.local_addr()?;
    let slots = Arc::new(Semaphore::new(MAX_RELAYED));
    tokio::spawn(async move {
        loop {
            let client = match listener.accept().await {
                Ok((client, _)) => client,
                Err(e) => {
                    tracing::warn!(error = %e, "egress relay cannot accept a connection");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                tracing::warn!("egress relay is at its connection limit");
                continue;
            };
            let permit = permit.clone();
            tokio::spawn(async move {
                let _slot = slot;
                if let Err(e) = relay(client, &permit).await {
                    tracing::debug!(error = %e, "egress relay connection ended");
                }
            });
        }
    });
    Ok(addr)
}

async fn relay(mut client: TcpStream, permit: &Permit) -> io::Result<()> {
    let upstream = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut client, permit))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SOCKS5 request timed out"))??;
    if let Some(mut upstream) = upstream {
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    }
    Ok(())
}

/// Read a SOCKS5 CONNECT request and open the permitted connection it names.
/// `None` when the request was refused; the client has had its reply.
async fn handshake(client: &mut TcpStream, permit: &Permit) -> io::Result<Option<TcpStream>> {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    if greeting[0] != VERSION {
        return Ok(None);
    }
    let mut methods = vec![0u8; usize::from(greeting[1])];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&NO_AUTH) {
        client.write_all(&[VERSION, NO_ACCEPTABLE_METHOD]).await?;
        return Ok(None);
    }
    client.write_all(&[VERSION, NO_AUTH]).await?;

    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    let [version, command, _, address_type] = request;
    if version != VERSION {
        return Ok(None);
    }
    let host = match address_type {
        ATYP_V4 => {
            let mut octets = [0u8; 4];
            client.read_exact(&mut octets).await?;
            Ipv4Addr::from(octets).to_string()
        }
        ATYP_V6 => {
            let mut octets = [0u8; 16];
            client.read_exact(&mut octets).await?;
            Ipv6Addr::from(octets).to_string()
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name).await?;
            match String::from_utf8(name) {
                Ok(name) => name,
                Err(_) => {
                    reply(client, REPLY_ADDRESS_NOT_SUPPORTED).await?;
                    return Ok(None);
                }
            }
        }
        _ => {
            reply(client, REPLY_ADDRESS_NOT_SUPPORTED).await?;
            return Ok(None);
        }
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    if command != CONNECT {
        reply(client, REPLY_COMMAND_NOT_SUPPORTED).await?;
        return Ok(None);
    }

    let addrs = match resolve_addrs_where(&host, port, |addr| permit(addr)).await {
        Ok(addrs) => addrs,
        Err(HostGuardError::Blocked(_)) => {
            tracing::warn!(host = %host, port, "egress guard refused a broker connection");
            reply(client, REPLY_NOT_ALLOWED).await?;
            return Ok(None);
        }
        Err(HostGuardError::Resolve(_)) => {
            reply(client, REPLY_HOST_UNREACHABLE).await?;
            return Ok(None);
        }
    };
    match connect_pinned_tcp(&addrs).await {
        Ok(upstream) => {
            reply(client, REPLY_OK).await?;
            Ok(Some(upstream))
        }
        Err(_) => {
            reply(client, REPLY_REFUSED).await?;
            Ok(None)
        }
    }
}

/// A SOCKS5 reply; the bound address is not used by clients, so it is zero.
async fn reply(client: &mut TcpStream, code: u8) -> io::Result<()> {
    client
        .write_all(&[VERSION, code, 0, ATYP_V4, 0, 0, 0, 0, 0, 0])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        addr
    }

    /// Sends a CONNECT for `host`:`port` as a domain name and returns the
    /// reply code with the stream.
    async fn connect_through(relay: SocketAddr, host: &str, port: u16) -> (u8, TcpStream) {
        let mut stream = TcpStream::connect(relay).await.unwrap();
        stream.write_all(&[VERSION, 1, NO_AUTH]).await.unwrap();
        let mut choice = [0u8; 2];
        stream.read_exact(&mut choice).await.unwrap();
        assert_eq!(choice, [VERSION, NO_AUTH]);
        let mut request = vec![VERSION, CONNECT, 0, ATYP_DOMAIN, host.len() as u8];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).await.unwrap();
        let mut response = [0u8; 10];
        stream.read_exact(&mut response).await.unwrap();
        (response[1], stream)
    }

    #[tokio::test]
    async fn a_permitted_target_is_relayed() {
        let target = echo_server().await;
        let relay = start(Arc::new(move |addr: &SocketAddr| *addr == target)).unwrap();

        let (code, mut stream) = connect_through(relay, "127.0.0.1", target.port()).await;
        assert_eq!(code, REPLY_OK);
        stream.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
    }

    #[tokio::test]
    async fn a_target_outside_the_policy_is_refused() {
        let target = echo_server().await;
        let relay = start(Arc::new(|_: &SocketAddr| false)).unwrap();

        let (code, _) = connect_through(relay, "127.0.0.1", target.port()).await;
        assert_eq!(code, REPLY_NOT_ALLOWED);
        let (code, _) = connect_through(relay, "localhost", target.port()).await;
        assert_eq!(code, REPLY_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn a_request_other_than_connect_is_refused() {
        let relay = start(Arc::new(|_: &SocketAddr| true)).unwrap();
        let mut stream = TcpStream::connect(relay).await.unwrap();
        stream.write_all(&[VERSION, 1, NO_AUTH]).await.unwrap();
        let mut choice = [0u8; 2];
        stream.read_exact(&mut choice).await.unwrap();
        // BIND to 127.0.0.1:80.
        stream
            .write_all(&[VERSION, 2, 0, ATYP_V4, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        let mut response = [0u8; 10];
        stream.read_exact(&mut response).await.unwrap();
        assert_eq!(response[1], REPLY_COMMAND_NOT_SUPPORTED);
    }
}
