use tokio::io::Result;
use tokio::net::{UdpSocket, udp::{RecvHalf, SendHalf}};
use tokio::sync::{RwLock, mpsc};
use std::net::{SocketAddr, Ipv4Addr};
use std::sync::Arc;
use super::{Event, SendLANEvent, Peer, PeerState, log_err, log_warn, ForwarderFrame, Parser, Packet};
use std::collections::HashMap;
use serde::Serialize;
use juniper::GraphQLObject;
use owning_ref::MutexGuardRef;

struct InnerPeerManager {
    /// real ip to peer map
    cache: HashMap<SocketAddr, Peer>,
    /// key is the inner ip in virtual LAN, value is cache's key
    /// a client may have more than one inner ip.
    map: HashMap<Ipv4Addr, SocketAddr>,

    send_half: SendHalf,
}

impl InnerPeerManager {
    fn new(send_half: SendHalf) -> Self {
        Self {
            cache: HashMap::new(),
            map: HashMap::new(),
            send_half,
        }
    }
}

pub struct PeerManager {
    inner: RwLock<InnerPeerManager>,
    ignore_idle: bool,
}

impl PeerManager {
    pub fn new(send_half: SendHalf, ignore_idle: bool) -> Self {
        Self {
            inner: RwLock::new(
                InnerPeerManager::new(send_half)
            ),
            ignore_idle,
        }
    }
    pub async fn peer_mut<F>(&self, addr: SocketAddr, event_send: mpsc::Sender<Event>, func: F) -> ()
    where
        F: FnOnce(&mut Peer) -> ()
    {
        let cache = &mut self.inner.write().await.cache;
        let peer = {
            if cache.get(&addr).is_none() {
                cache.insert(
                    addr,
                    Peer::new(addr, event_send)
                );
            }
            cache.get_mut(&addr).unwrap()
        };
        func(peer)
    }
    pub async fn send_lan(&self, from: SocketAddr, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, packet: Vec<u8>) -> () {
        let inner = &mut self.inner.write().await;
        inner.map.insert(src_ip, from);
        if let Some(addr) = inner.map.get(&dst_ip) {
            log_err(inner.send_half.send_to(&packet, addr).await, "failed to send unary packet");
        } else {
            for (addr, _) in inner.cache.iter()
                .filter(|(_, i)| !self.ignore_idle || i.state.is_connected())
                .filter(|(addr, _) | &&from != addr)
            {
                log_err(
                    inner.send_half.send_to(&packet, &addr).await,
                    &format!("failed to send to {} when broadcasting", addr)
                );
            }
        }
    }
}
