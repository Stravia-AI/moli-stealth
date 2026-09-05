//! Async DNS and Happy Eyeballs TCP connection for outbound transports.

use std::{io, net::SocketAddr, time::Duration};

use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::net::TcpStream;

use crate::TransportError;

const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);

/// Connects to an origin. A supplied address slice is authoritative: its exact
/// socket addresses are attempted and DNS is never consulted. Without a pin,
/// DNS resolution is asynchronous and all returned addresses are eligible.
pub(crate) async fn connect_target(
    host: &str,
    port: u16,
    resolved_addresses: Option<&[SocketAddr]>,
) -> Result<TcpStream, TransportError> {
    let addresses = match resolved_addresses {
        Some([]) => {
            return Err(TransportError::Resolve(
                "the approved address list is empty".into(),
            ));
        }
        Some(addresses) => addresses.to_vec(),
        None => tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| TransportError::Resolve(error.to_string()))?
            .collect(),
    };
    if addresses.is_empty() {
        return Err(TransportError::Resolve(format!(
            "no addresses resolved for {host}"
        )));
    }

    let ordered = interleave_address_families(addresses);
    let mut attempts = FuturesUnordered::new();
    for (index, address) in ordered.into_iter().enumerate() {
        attempts.push(async move {
            if index != 0 {
                tokio::time::sleep(HAPPY_EYEBALLS_DELAY * index as u32).await;
            }
            TcpStream::connect(address).await
        });
    }

    let mut last_error = None;
    while let Some(result) = attempts.next().await {
        match result {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(TransportError::Io(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no connection addresses")
    })))
}

fn interleave_address_families(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let prefer_ipv6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let (mut ipv6, mut ipv4): (Vec<_>, Vec<_>) =
        addresses.into_iter().partition(SocketAddr::is_ipv6);
    ipv6.reverse();
    ipv4.reverse();

    let mut ordered = Vec::with_capacity(ipv6.len() + ipv4.len());
    while !ipv6.is_empty() || !ipv4.is_empty() {
        if prefer_ipv6 {
            if let Some(address) = ipv6.pop() {
                ordered.push(address);
            }
            if let Some(address) = ipv4.pop() {
                ordered.push(address);
            }
        } else {
            if let Some(address) = ipv4.pop() {
                ordered.push(address);
            }
            if let Some(address) = ipv6.pop() {
                ordered.push(address);
            }
        }
    }
    ordered
}
