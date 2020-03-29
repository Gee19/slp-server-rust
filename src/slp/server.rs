use tokio::io::Result;
use tokio::net::{UdpSocket, udp::RecvHalf};
use tokio::sync::{RwLock, mpsc};
use std::net::{SocketAddr, Ipv4Addr};
use std::sync::Arc;
use super::{Event, SendLANEvent, Peer, log_err, log_warn, ForwarderFrame, Parser, PeerManager};
use std::collections::HashMap;
use serde::Serialize;
use juniper::GraphQLObject;

/// Infomation about this server
#[derive(Clone, Debug, Eq, PartialEq, Serialize, GraphQLObject)]
pub struct ServerInfo {
    /// The number of online clients
    online: i32,
    /// The number of idle clients(not sending packets for 30s)
    idle: i32,
    /// The version of the server
    version: String,
}

struct InnerServer {
    cache: HashMap<SocketAddr, Peer>,
    map: HashMap<Ipv4Addr, SocketAddr>,
    config: UDPServerConfig,
}
impl InnerServer {
    fn new(config: UDPServerConfig) -> Self {
        Self {
            cache: HashMap::new(),
            map: HashMap::new(),
            config,
        }
    }

    fn server_info(&self) -> ServerInfo {
        let online = self.cache.len() as i32;
        let idle = self.cache.values().filter(|i| i.state.is_idle()).count() as i32;
        ServerInfo {
            online,
            idle,
            version: std::env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

pub struct UDPServerConfig {
    ignore_idle: bool,
}

#[derive(Clone)]
pub struct UDPServer {
    inner: Arc<RwLock<InnerServer>>,
    peer_manager: Arc<PeerManager>,
}

impl UDPServer {
    pub async fn new(addr: &str, config: UDPServerConfig) -> Result<Self> {
        let inner = Arc::new(RwLock::new(InnerServer::new(config)));
        let inner2 = inner.clone();
        let inner3 = inner.clone();
        let (event_send, mut event_recv) = mpsc::channel::<Event>(100);
        let (recv_half, mut send_half) = UdpSocket::bind(addr).await?.split();

        tokio::spawn(async {
            if let Err(err) = Self::recv(recv_half, inner2, event_send).await {
                log::error!("recv thread exited. reason: {:?}", err);
            }
        });
        tokio::spawn(async move {
            let inner = inner3;
            while let Some(event) = event_recv.recv().await {
                let inner = &mut inner.write().await;
                match event {
                    Event::Close(addr) => {
                        inner.cache.remove(&addr);
                    },
                    Event::SendLAN(SendLANEvent{
                        from,
                        src_ip,
                        dst_ip,
                        packet
                    }) => {
                        inner.map.insert(src_ip, from);
                        if let Some(addr) = inner.map.get(&dst_ip) {
                            log_err(send_half.send_to(&packet, addr).await, "failed to send unary packet");
                        } else {
                            for (addr, _) in inner.cache.iter()
                                .filter(|(_, i)| !inner.config.ignore_idle || i.state.is_connected())
                                .filter(|(addr, _) | &&from != addr)
                            {
                                log_err(
                                    send_half.send_to(&packet, &addr).await,
                                    &format!("failed to send to {} when broadcasting", addr)
                                );
                            }
                        }
                    },
                    Event::SendClient(addr, packet) => {
                        log_err(send_half.send_to(&packet, &addr).await, "failed to sendclient");
                    }
                }
            }
            log::error!("event down");
        });

        Ok(Self {
            inner,
            peer_manager: Arc::new(PeerManager::new(send_half, config.ignore_idle)),
        })
    }
    async fn recv(mut recv: RecvHalf, inner: Arc<RwLock<InnerServer>>, mut event_send: mpsc::Sender<Event>) -> Result<()> {
        loop {
            let mut buffer = vec![0u8; 65536];
            let (size, addr) = recv.recv_from(&mut buffer).await?;
            buffer.truncate(size);

            let frame = match ForwarderFrame::parse(&buffer) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if let ForwarderFrame::Ping(ping) = &frame {
                log_warn(
                    event_send.try_send(Event::SendClient(addr, ping.build())),
                    "failed to send pong"
                );
                continue
            }
            let cache = &mut inner.write().await.cache;
            let peer = {
                if cache.get(&addr).is_none() {
                    cache.insert(
                        addr,
                        Peer::new(addr, event_send.clone())
                    );
                }
                cache.get_mut(&addr).unwrap()
            };
            peer.on_packet(buffer);
        }
    }
    pub async fn server_info(&self) -> ServerInfo {
        let inner = self.inner.read().await;

        inner.server_info()
    }
}


pub struct UDPServerBuilder(UDPServerConfig);

impl UDPServerBuilder {
    pub fn new() -> UDPServerBuilder {
        UDPServerBuilder(UDPServerConfig {
            ignore_idle: false,
        })
    }
    pub fn ignore_idle(mut self, v: bool) -> Self {
        self.0.ignore_idle = v;
        self
    }
    pub async fn build(self, addr: &str) -> Result<UDPServer> {
        UDPServer::new(addr, self.0).await
    }
}
