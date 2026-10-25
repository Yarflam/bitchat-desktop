//! Mesh link layer: drives the BLE central, announces the node, establishes
//! Noise sessions, and exchanges private messages — byte-compatible with the
//! Android client's mesh orchestration (`MeshCore`, `MessageHandler`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitchat_ble::central::{CentralCommand, CentralEventOut, CentralHandle};
#[cfg(windows)]
use bitchat_ble::peripheral::{PeripheralCommand, PeripheralEvent, PeripheralHandle};
use bitchat_ble::PeripheralId;
use bitchat_wire::announce::{IdentityAnnouncement, PeerCapabilities};
use bitchat_wire::gcs;
use bitchat_wire::identity::verify_packet;
use bitchat_wire::noise::{derive_peer_id, NoiseSession};
use bitchat_wire::noise_payload::{
    AuthenticatedPeerState, NoisePayload, NoisePayloadType, PrivateMessagePacket,
};
use bitchat_wire::sync::RequestSyncPacket;
use bitchat_wire::{MessageType, Packet};
use tokio::sync::mpsc;

use crate::identity::NodeIdentity;
use crate::LinkError;

/// Where a peer is reachable: BLE central link, our GATT server, or both.
#[derive(Clone, Default)]
struct Route {
    central: Option<PeripheralId>,
    peripheral: Option<String>,
}

impl Route {
    fn is_empty(&self) -> bool {
        self.central.is_none() && self.peripheral.is_none()
    }
}

/// Default TTL used by the Android client for regular packets.
const MAX_TTL: u8 = 7;
/// Sync packets stay neighbor-only, mirror of `AppConstants.SYNC_TTL_HOPS`.
const SYNC_TTL: u8 = 0;
/// Mesh-wide recipient, port of `SpecialRecipients.BROADCAST` (8 x 0xFF).
pub const BROADCAST_RECIPIENT: [u8; 8] = [0xff; 8];
/// Announcements older than this are pruned from the gossip store, mirror of
/// `AppConstants.Mesh.STALE_PEER_TIMEOUT_MS`.
const STALE_ANNOUNCE_MS: u64 = 180_000;
/// Initial targeted sync delay after a peer's first verified announce,
/// mirror of `GossipSyncManager.scheduleInitialSyncToPeer`.
const INITIAL_SYNC_DELAY: Duration = Duration::from_secs(5);
/// Periodic sync broadcast period (`GossipSyncManager.start`).
const SYNC_PERIOD: Duration = Duration::from_secs(30);
/// Drop a session after this many consecutive decrypt failures, like the
/// Android client does before re-handshaking.
const MAX_DECRYPT_FAILURES: u32 = 3;

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Application-facing link events.
#[derive(Debug)]
pub enum LinkEvent {
    /// A peer announced itself (signature verified when present).
    PeerAnnounced {
        peer_id: String,
        nickname: String,
        verified: bool,
        capabilities: Option<u16>,
    },
    /// Noise XX session with the peer is established in both directions.
    SessionEstablished {
        peer_id: String,
    },
    /// Decrypted private chat message from a peer.
    PrivateMessage {
        peer_id: String,
        message_id: String,
        content: String,
    },
    /// Delivery acknowledgement for one of our messages.
    Delivered {
        peer_id: String,
        message_id: String,
    },
    /// We sent our AuthenticatedPeerState right after session establishment.
    PeerStateSent {
        peer_id: String,
    },
    /// AuthenticatedPeerState received from a peer.
    PeerState {
        peer_id: String,
        capabilities: u64,
        signing_public_key: String,
    },
    PeerDisconnected {
        peer_id: String,
    },
    /// A broadcast (mesh-wide) message from a peer.
    BroadcastMessage {
        peer_id: String,
        content: String,
    },
    Info(String),
}

/// Commands accepted by a running [`MeshLink`].
#[derive(Debug)]
pub enum LinkCommand {
    SendPrivateMessage { peer_id: String, content: String },
    /// Mesh-wide broadcast message.
    SendBroadcastMessage { content: String },
    /// Change our nickname and re-announce to every route immediately.
    SetNickname { nickname: String },
    /// Send a gossip sync request; `None` broadcasts to all neighbors.
    SendSyncRequest { target: Option<String> },
    Shutdown,
}

/// A running mesh node. Owns the central actor task plus session state.
pub struct MeshLink {
    pub events: mpsc::Receiver<LinkEvent>,
    commands: mpsc::Sender<LinkCommand>,
}

impl MeshLink {
    pub async fn start(
        identity: NodeIdentity,
        nickname: &str,
        channel: usize,
    ) -> Result<Self, LinkError> {
        let central = bitchat_ble::central::run_central(channel).await?;
        Ok(Self::with_central(identity, nickname, central))
    }

    /// Wrap an already-running central (useful for tests and custom setups).
    pub fn with_central(identity: NodeIdentity, nickname: &str, central: CentralHandle) -> Self {
        let (command_tx, command_rx) = mpsc::channel(64);
        let (event_tx, event_rx) = mpsc::channel(256);
        let CentralHandle {
            events: central_events,
            commands: central_commands,
        } = central;

        // The desktop node participates in both roles, like the Android
        // client: BLE central (btleplug) plus GATT server (WinRT).
        let (peripheral, peripheral_events) = spawn_peripheral(identity.peer_id_bytes());

        let mut actor = Actor {
            identity,
            nickname: nickname.to_string(),
            central_events,
            central_commands: central_commands.clone(),
            peripheral,
            peripheral_events,
            commands: command_rx,
            internal_tx: command_tx.clone(),
            events: event_tx,
            sessions: HashMap::new(),
            routes: HashMap::new(),
            peers: HashMap::new(),
            verified: HashMap::new(),
            pending: HashMap::new(),
            decrypt_failures: HashMap::new(),
            peerstate_sent: HashSet::new(),
            connecting: HashSet::new(),
            gossip_announcements: HashMap::new(),
            gossip_announcement_order: VecDeque::new(),
            gossip_messages: VecDeque::new(),
            seen_packet_ids: HashSet::new(),
            seen_packet_order: VecDeque::new(),
            initial_sync_scheduled: HashSet::new(),
            sync_tick: tokio::time::interval_at(
                tokio::time::Instant::now() + SYNC_PERIOD,
                SYNC_PERIOD,
            ),
        };
        tokio::spawn(async move { actor.run().await });

        Self {
            events: event_rx,
            commands: command_tx,
        }
    }

    pub async fn send_private_message(
        &self,
        peer_id: &str,
        content: &str,
    ) -> Result<(), LinkError> {
        self.commands
            .send(LinkCommand::SendPrivateMessage {
                peer_id: peer_id.to_string(),
                content: content.to_string(),
            })
            .await
            .map_err(|_| LinkError::Closed)
    }

    /// Send a mesh-wide broadcast message (the common channel).
    pub async fn send_broadcast_message(&self, content: &str) -> Result<(), LinkError> {
        self.commands
            .send(LinkCommand::SendBroadcastMessage {
                content: content.to_string(),
            })
            .await
            .map_err(|_| LinkError::Closed)
    }

    /// Change our nickname; peers see it after the re-announcement.
    pub async fn set_nickname(&self, nickname: &str) -> Result<(), LinkError> {
        self.commands
            .send(LinkCommand::SetNickname {
                nickname: nickname.to_string(),
            })
            .await
            .map_err(|_| LinkError::Closed)
    }

    pub async fn shutdown(&self) {
        let _ = self.commands.send(LinkCommand::Shutdown).await;
    }
}

struct Actor {
    identity: NodeIdentity,
    nickname: String,
    central_events: mpsc::Receiver<CentralEventOut>,
    central_commands: mpsc::Sender<CentralCommand>,
    #[cfg(windows)]
    peripheral: Option<PeripheralHandle>,
    /// Event stream from our GATT server; a never-fed dummy when absent so
    /// the select! branch stays structurally alive.
    peripheral_events: mpsc::Receiver<PeripheralEvent>,
    commands: mpsc::Receiver<LinkCommand>,
    /// Clone of the public command sender, used to schedule internal timers.
    internal_tx: mpsc::Sender<LinkCommand>,
    events: mpsc::Sender<LinkEvent>,
    /// Wire peer ID -> Noise session.
    sessions: HashMap<String, NoiseSession>,
    /// Wire peer ID -> how we reach it.
    routes: HashMap<String, Route>,
    /// Latest verified announcement per peer.
    peers: HashMap<String, IdentityAnnouncement>,
    /// Whether the announcement's Ed25519 signature checked out.
    verified: HashMap<String, bool>,
    /// Messages queued while a handshake is in flight.
    pending: HashMap<String, Vec<(String, String)>>,
    decrypt_failures: HashMap<String, u32>,
    /// Peers that already received our AuthenticatedPeerState for the current
    /// Noise session generation; re-handshake clears the entry so we resend.
    peerstate_sent: HashSet<String>,
    connecting: HashSet<PeripheralId>,
    /// Gossip store: latest announcement per sender peer ID.
    gossip_announcements: HashMap<String, ([u8; 16], Packet)>,
    /// Insertion order of `gossip_announcements` for capacity eviction.
    gossip_announcement_order: VecDeque<String>,
    /// Broadcast messages in insertion order (capped, deduped by packet ID).
    gossip_messages: VecDeque<([u8; 16], Packet)>,
    /// Packet IDs already seen/handled (loop prevention), and their order.
    seen_packet_ids: HashSet<[u8; 16]>,
    seen_packet_order: VecDeque<[u8; 16]>,
    /// Peers for which the initial targeted sync has been scheduled.
    initial_sync_scheduled: HashSet<String>,
    /// Periodic gossip sync broadcast.
    sync_tick: tokio::time::Interval,
}

impl Actor {
    async fn run(&mut self) {
        loop {
            tokio::select! {
                event = self.central_events.recv() => match event {
                    Some(event) => self.handle_central(event).await,
                    None => break,
                },
                event = self.peripheral_events.recv() => match event {
                    Some(event) => self.handle_peripheral(event).await,
                    None => self.park_peripheral_events(),
                },
                command = self.commands.recv() => match command {
                    Some(LinkCommand::SendPrivateMessage { peer_id, content }) => {
                        self.send_private_message(&peer_id, &content).await;
                    }
                    Some(LinkCommand::SendSyncRequest { target }) => {
                        self.send_request_sync(target).await;
                    }
                    Some(LinkCommand::SendBroadcastMessage { content }) => {
                        self.send_broadcast_message(&content).await;
                    }
                    Some(LinkCommand::SetNickname { nickname }) => {
                        let nickname = nickname.trim();
                        if !nickname.is_empty() && nickname != self.nickname {
                            self.nickname = nickname.to_string();
                            let routes: Vec<Route> = self.routes.values().cloned().collect();
                            for route in routes {
                                self.send_announce_to(&route).await;
                            }
                            self.info(format!("nickname changed to {nickname}; re-announced"));
                        }
                    }
                    Some(LinkCommand::Shutdown) | None => break,
                },
                _ = self.sync_tick.tick() => {
                    self.prune_stale_gossip();
                    self.send_request_sync(None).await;
                },
            }
        }
        let _ = self.central_commands.send(CentralCommand::Shutdown).await;
        #[cfg(windows)]
        if let Some(peripheral) = &self.peripheral {
            let _ = peripheral.send(PeripheralCommand::Shutdown);
        }
    }

    /// Replace a dead peripheral event stream with a never-fed channel so the
    /// select! branch no longer fires.
    fn park_peripheral_events(&mut self) {
        self.peripheral_events = mpsc::channel(1).1;
    }

    #[cfg(windows)]
    async fn handle_peripheral(&mut self, event: PeripheralEvent) {
        match event {
            PeripheralEvent::AdvertisingStarted => {
                self.info("GATT server advertising (phone can connect to us)");
            }
            PeripheralEvent::AdvertisingFailed(reason) => {
                self.info(format!("GATT server advertising failed: {reason}"));
            }
            PeripheralEvent::Subscribed { session } => {
                self.info("a peer connected to our GATT server".to_string());
                self.send_announce_to(&Route {
                    central: None,
                    peripheral: Some(session),
                })
                .await;
            }
            PeripheralEvent::Unsubscribed { session } => {
                let lost: Vec<String> = self
                    .routes
                    .iter()
                    .filter(|(_, route)| route.peripheral.as_deref() == Some(session.as_str()))
                    .map(|(wire_id, _)| wire_id.clone())
                    .collect();
                for wire_id in lost {
                    if let Some(route) = self.routes.get_mut(&wire_id) {
                        route.peripheral = None;
                        if route.is_empty() {
                            self.routes.remove(&wire_id);
                            self.sessions.remove(&wire_id);
                            self.decrypt_failures.remove(&wire_id);
                            let _ = self
                                .events
                                .try_send(LinkEvent::PeerDisconnected { peer_id: wire_id });
                        }
                    }
                }
            }
            PeripheralEvent::Packet {
                session,
                peer_id,
                packet,
            } => {
                self.routes.entry(peer_id.clone()).or_default().peripheral = Some(session.clone());
                let via = Route {
                    central: None,
                    peripheral: Some(session),
                };
                self.handle_packet_route(&peer_id, packet, via).await;
            }
            PeripheralEvent::Info(message) => self.info(format!("GATT: {message}")),
        }
    }

    fn info(&self, message: impl Into<String>) {
        let _ = self.events.try_send(LinkEvent::Info(message.into()));
    }

    async fn handle_central(&mut self, event: CentralEventOut) {
        match event {
            CentralEventOut::Discovered(info) => {
                if let Some(advertised) = &info.advertised_peer_id {
                    let wire_id = hex(advertised);
                    if !self.routes.contains_key(&wire_id) && !self.connecting.contains(&info.id) {
                        self.connecting.insert(info.id.clone());
                        self.info(format!("connecting to {} ({wire_id})...", info.address));
                        let _ = self
                            .central_commands
                            .send(CentralCommand::Connect(info.id))
                            .await;
                    }
                }
            }
            CentralEventOut::Connected { id, info } => {
                self.connecting.remove(&id);
                self.info(format!("connected to {}", info.address));
                self.send_announce_to(&Route {
                    central: Some(id),
                    peripheral: None,
                })
                .await;
            }
            CentralEventOut::Disconnected { id } => {
                self.connecting.remove(&id);
                let lost: Vec<String> = self
                    .routes
                    .iter()
                    .filter(|(_, route)| route.central.as_ref() == Some(&id))
                    .map(|(w, _)| w.clone())
                    .collect();
                for wire_id in lost {
                    if let Some(route) = self.routes.get_mut(&wire_id) {
                        route.central = None;
                        if route.is_empty() {
                            self.routes.remove(&wire_id);
                            self.sessions.remove(&wire_id);
                            self.decrypt_failures.remove(&wire_id);
                            let _ = self
                                .events
                                .try_send(LinkEvent::PeerDisconnected { peer_id: wire_id });
                        }
                    }
                }
            }
            CentralEventOut::Packet {
                id,
                peer_id,
                packet,
            } => {
                let via = Route {
                    central: Some(id),
                    peripheral: None,
                };
                self.routes.entry(peer_id.clone()).or_default().central = via.central.clone();
                self.handle_packet_route(&peer_id, packet, via).await;
            }
            CentralEventOut::AdapterLost(reason) => self.info(format!("BLE adapter: {reason}")),
        }
    }

    async fn handle_packet_route(&mut self, sender: &str, packet: Packet, via: Route) {
        if std::env::var_os("BITCHATD_DEBUG_HEX").is_some() {
            let frame = bitchat_wire::packet::encode(&packet);
            eprintln!(
                "[debug-hex] rx type={:#04x} from={sender} len={} {}",
                packet.packet_type,
                frame.as_ref().map_or(0, |f| f.len()),
                frame.as_deref().map(to_hex).unwrap_or_default()
            );
        }
        if sender == self.identity.peer_id {
            return; // our own echo
        }
        if hex(&packet.sender_id) != sender {
            self.info(format!("packet sender field mismatch from {sender}"));
            return;
        }
        let Some(message_type) = MessageType::from_value(packet.packet_type) else {
            self.info(format!(
                "ignoring unknown packet type {:#04x}",
                packet.packet_type
            ));
            return;
        };

        match message_type {
            MessageType::Announce => self.handle_announce(sender.to_string(), packet, via).await,
            MessageType::Message => self.handle_broadcast_message(sender, packet, via).await,
            MessageType::RequestSync => self.handle_request_sync(sender, &packet),
            MessageType::NoiseHandshake => self.handle_noise_handshake(sender, packet, via).await,
            MessageType::NoiseEncrypted => self.handle_noise_encrypted(sender, packet).await,
            other => self.info(format!(
                "ignoring {other:?} from {sender} (not implemented yet)"
            )),
        }
    }

    /// Record a public packet ID in the seen cache (loop prevention),
    /// returning `false` when the packet was already seen.
    fn mark_seen(&mut self, id: [u8; 16]) -> bool {
        if !self.seen_packet_ids.insert(id) {
            return false;
        }
        self.seen_packet_order.push_back(id);
        while self.seen_packet_order.len() > gcs::DEDUP_CAPACITY {
            if let Some(evicted) = self.seen_packet_order.pop_front() {
                self.seen_packet_ids.remove(&evicted);
            }
        }
        true
    }

    /// Deterministic sync ID for a public packet (`PacketIdUtil`).
    fn packet_sync_id(packet: &Packet) -> [u8; 16] {
        gcs::packet_id(
            packet.packet_type,
            &packet.sender_id,
            packet.timestamp,
            &packet.payload,
        )
    }

    async fn handle_announce(&mut self, sender: String, packet: Packet, via: Route) {
        let Some(announce) = IdentityAnnouncement::decode(&packet.payload) else {
            self.info(format!("undecodable ANNOUNCE from {sender}"));
            return;
        };
        let noise_id = derive_peer_id(
            &announce
                .noise_public_key
                .clone()
                .try_into()
                .unwrap_or([0u8; 32]),
        );
        if noise_id != sender {
            self.info(format!(
                "ANNOUNCE from {sender} carries mismatched noise key (peer {noise_id}); rejected"
            ));
            return;
        }
        let signing_key: [u8; 32] = announce
            .signing_public_key
            .clone()
            .try_into()
            .unwrap_or([0u8; 32]);
        let is_verified = packet
            .signature
            .as_ref()
            .map(|_| verify_packet(&packet, &signing_key))
            .unwrap_or(false);
        self.peers.insert(sender.clone(), announce.clone());
        self.verified.insert(sender.clone(), is_verified);

        if is_verified {
            // Gossip: only verified announcements enter the sync store and
            // get relayed, mirroring the phone's validPacket gate.
            let id = Self::packet_sync_id(&packet);
            if self.mark_seen(id) {
                self.store_announcement(&sender, id, packet.clone());
                self.relay_public_packet(&packet, &via);
            }
            self.schedule_initial_sync(&sender);
        }

        let _ = self.events.try_send(LinkEvent::PeerAnnounced {
            peer_id: sender.clone(),
            nickname: announce.nickname.clone(),
            verified: is_verified,
            capabilities: announce.capabilities.map(|c| c.0 as u16),
        });

        if via.central.is_none() && via.peripheral.is_some() {
            // Prefer a single link per peer, mirroring the phone client's
            // duplicate-connection avoidance: two GATT sessions to the same
            // device fight over one BLE ACL, churning both. When the peer
            // connects to our GATT server, drop our outgoing central link.
            let central_id = self.routes.get(&sender).and_then(|r| r.central.clone());
            if let Some(id) = central_id {
                self.info(format!(
                    "{sender} on our GATT server; dropping our central link (single-link preference)"
                ));
                let _ = self.central_commands.send(CentralCommand::Disconnect(id)).await;
            }
            // The phone client only starts its Noise handshake when the user
            // sends a message (dropping that first message), so when a peer
            // connected to our GATT server we take the initiative instead.
            // The phone answers an incoming 0x10 even with an existing
            // session, so the channel is ready before the user types.
            self.initiate_handshake(&sender).await;
        }
    }

    /// Insert an announcement into the gossip store, evicting the oldest
    /// entry when over capacity (`GossipSyncManager.onPublicPacketSeen`).
    fn store_announcement(&mut self, sender: &str, id: [u8; 16], packet: Packet) {
        if self.gossip_announcements.contains_key(sender) {
            self.gossip_announcements
                .insert(sender.to_string(), (id, packet));
            return;
        }
        while self.gossip_announcements.len() >= gcs::SEEN_CAPACITY {
            let Some(oldest) = self.gossip_announcement_order.pop_front() else {
                break;
            };
            self.gossip_announcements.remove(&oldest);
        }
        self.gossip_announcement_order.push_back(sender.to_string());
        self.gossip_announcements
            .insert(sender.to_string(), (id, packet));
    }

    /// Schedule the one-shot targeted sync sent shortly after a peer first
    /// announces itself (`GossipSyncManager.scheduleInitialSyncToPeer`).
    fn schedule_initial_sync(&mut self, peer_id: &str) {
        if !self.initial_sync_scheduled.insert(peer_id.to_string()) {
            return;
        }
        let tx = self.internal_tx.clone();
        let peer = peer_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(INITIAL_SYNC_DELAY).await;
            let _ = tx
                .send(LinkCommand::SendSyncRequest {
                    target: Some(peer),
                })
                .await;
        });
    }

    /// Handle a broadcast mesh message: store, surface, and relay.
    async fn handle_broadcast_message(&mut self, sender: &str, packet: Packet, via: Route) {
        // Broadcast means no recipient or the all-0xFF pseudo-recipient,
        // mirroring `PacketProcessor`'s isBroadcast check.
        let is_broadcast =
            packet.recipient_id.is_none_or(|recipient| recipient == BROADCAST_RECIPIENT);
        if !is_broadcast {
            // Directed messages ride Noise; unexpected on the clear channel.
            self.info(format!("ignoring directed MESSAGE from {sender}"));
            return;
        }
        // Relay/storage requires a signature that verifies against the
        // sender's announced key, mirroring the phone's validPacket gate.
        let Some(announce) = self.peers.get(sender) else {
            return;
        };
        let Ok(signing_key) = announce.signing_public_key.clone().try_into() else {
            return;
        };
        if self.verified.get(sender) != Some(&true) || !verify_packet(&packet, &signing_key) {
            return;
        }
        let id = Self::packet_sync_id(&packet);
        if !self.mark_seen(id) {
            return;
        }
        self.store_broadcast_message(id, packet.clone());
        // The broadcast payload is the raw UTF-8 text (`sendMessage`).
        let content = String::from_utf8_lossy(&packet.payload).into_owned();
        let _ = self.events.try_send(LinkEvent::BroadcastMessage {
            peer_id: sender.to_string(),
            content,
        });
        self.relay_public_packet(&packet, &via);
    }

    /// Send a mesh-wide broadcast message (`BluetoothMeshService.sendMessage`).
    async fn send_broadcast_message(&mut self, content: &str) {
        let mut packet = Packet::new(
            MessageType::Message as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            content.as_bytes().to_vec(),
        );
        packet.timestamp = now_ms();
        packet.recipient_id = Some(BROADCAST_RECIPIENT);
        packet.signature = self.identity.signing.sign_packet(&packet);

        // Track our own broadcast for gossip sync, then flood our routes.
        let id = Self::packet_sync_id(&packet);
        self.mark_seen(id);
        self.store_broadcast_message(id, packet.clone());
        let routes: Vec<Route> = self.routes.values().cloned().collect();
        for route in routes {
            self.send_frame(&route, &packet);
        }
    }

    fn store_broadcast_message(&mut self, id: [u8; 16], packet: Packet) {
        while self.gossip_messages.len() >= gcs::SEEN_CAPACITY {
            self.gossip_messages.pop_front();
        }
        self.gossip_messages.push_back((id, packet));
    }

    /// Drop announcements (and their messages) older than the stale timeout.
    fn prune_stale_gossip(&mut self) {
        let now = now_ms();
        let stale: Vec<String> = self
            .gossip_announcements
            .iter()
            .filter(|(_, (_, packet))| now.saturating_sub(packet.timestamp) > STALE_ANNOUNCE_MS)
            .map(|(peer, _)| peer.clone())
            .collect();
        for peer in &stale {
            self.gossip_announcements.remove(peer);
        }
        if stale.is_empty() {
            return;
        }
        self.gossip_announcement_order
            .retain(|peer| !stale.contains(peer));
        let stale_senders: HashSet<String> = stale.into_iter().collect();
        self.gossip_messages
            .retain(|(_, packet)| !stale_senders.contains(&hex(&packet.sender_id)));
    }

    /// Respond to a REQUEST_SYNC by sending the packets the requester is
    /// missing, announcements first (`GossipSyncManager.handleRequestSync`).
    fn handle_request_sync(&mut self, sender: &str, packet: &Packet) {
        let Some(request) = RequestSyncPacket::decode(&packet.payload) else {
            self.info(format!("undecodable REQUEST_SYNC from {sender}"));
            return;
        };
        let m = u64::from(request.m);
        let sorted = gcs::decode_to_sorted_set(request.p, m, &request.data);
        let might_contain = |id: &[u8; 16]| {
            let v = gcs::h64(id) % m;
            let v = if v == 0 { 1 } else { v };
            gcs::contains(&sorted, v)
        };

        let (offered, _) = self.build_sync_snapshot();
        let mut missing: Vec<([u8; 16], Packet)> = offered
            .into_iter()
            .filter(|(id, _)| !might_contain(id))
            .collect();
        missing.sort_by(|a, b| {
            let rank = |packet: &Packet| {
                if packet.packet_type == MessageType::Announce as u8 {
                    0
                } else {
                    1
                }
            };
            rank(&a.1)
                .cmp(&rank(&b.1))
                .then(a.1.timestamp.cmp(&b.1.timestamp))
                .then(a.0.cmp(&b.0))
        });
        if missing.is_empty() {
            return;
        }
        let Some(route) = self.routes.get(sender).cloned() else {
            return;
        };
        if route.is_empty() {
            return;
        }
        self.info(format!(
            "sync: offering {} packet(s) to {sender}",
            missing.len()
        ));

        // The packets are already signed by their originators; relay them
        // verbatim with the neighbor-only TTL, paced like the phone client.
        let central_tx = self.central_commands.clone();
        #[cfg(windows)]
        let peripheral = self.peripheral.clone();
        tokio::spawn(async move {
            for (index, (_, mut packet)) in missing.into_iter().enumerate() {
                if index > 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                packet.ttl = SYNC_TTL;
                if let Some(id) = &route.central {
                    let _ = central_tx
                        .send(CentralCommand::Send {
                            id: id.clone(),
                            packet: packet.clone(),
                        })
                        .await;
                }
                #[cfg(windows)]
                if let (Some(session), Some(handle)) = (&route.peripheral, &peripheral) {
                    let _ = handle.send(PeripheralCommand::Send {
                        session: session.clone(),
                        packet,
                    });
                }
            }
        });
    }

    /// Build the advertised/offered sync window, mirror of
    /// `GossipSyncManager.buildSyncSnapshot` (dependency closure included).
    fn build_sync_snapshot(&self) -> (Vec<([u8; 16], Packet)>, gcs::GcsParams) {
        let mut candidates: Vec<([u8; 16], Packet)> =
            self.gossip_announcements.values().cloned().collect();
        candidates.extend(self.gossip_messages.iter().cloned());
        candidates.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp).then(a.0.cmp(&b.0)));

        let limit = gcs::SEEN_CAPACITY
            .max(1)
            .min(gcs::estimate_max_elements_for_size(
                gcs::DEFAULT_FILTER_BYTES,
                gcs::derive_p(gcs::DEFAULT_FPR),
            ));
        let mut selected: Vec<([u8; 16], Packet)> = Vec::new();
        let mut have: HashSet<[u8; 16]> = HashSet::new();
        for (id, packet) in &candidates {
            if have.contains(id) {
                continue;
            }
            let dependency: Option<([u8; 16], Packet)> =
                if packet.packet_type == MessageType::Message as u8 {
                    let sender = hex(&packet.sender_id);
                    self.gossip_announcements
                        .get(&sender)
                        .filter(|(announcement_id, _)| !have.contains(announcement_id))
                        .cloned()
                } else {
                    None
                };
            let required = if dependency.is_some() { 2 } else { 1 };
            if have.len() + required > limit {
                continue;
            }
            if let Some(dep) = dependency {
                have.insert(dep.0);
                selected.push(dep);
            }
            have.insert(*id);
            selected.push((id.to_owned(), packet.clone()));
        }

        let filter = gcs::build_filter(
            &selected.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            gcs::DEFAULT_FILTER_BYTES,
            gcs::DEFAULT_FPR,
        );
        let encoded_count = ((filter.m >> filter.p) as usize).min(selected.len());
        selected.truncate(encoded_count);
        (selected, filter)
    }

    /// Broadcast (or targeted) gossip sync request, signed like any other
    /// packet we originate (`GossipSyncManager.sendRequestSync`).
    async fn send_request_sync(&mut self, target: Option<String>) {
        let (_, filter) = self.build_sync_snapshot();
        let payload = RequestSyncPacket::new(
            filter.p,
            filter.m.max(1).min(u64::from(u32::MAX)) as u32,
            filter.data,
        )
        .encode();
        let mut packet = Packet::new(
            MessageType::RequestSync as u8,
            SYNC_TTL,
            self.identity.peer_id_bytes(),
            payload,
        );
        packet.timestamp = now_ms();
        packet.recipient_id = target.as_deref().and_then(peer_bytes);

        match target {
            Some(peer) => {
                let Some(route) = self.routes.get(&peer).cloned() else {
                    return;
                };
                if route.is_empty() {
                    return;
                }
                self.sign_and_send(&route, packet);
            }
            None => {
                let routes: Vec<Route> = self.routes.values().cloned().collect();
                for route in routes {
                    self.sign_and_send(&route, packet.clone());
                }
            }
        }
    }

    /// Relay a valid public packet to every route except the arrival channel,
    /// with TTL decremented (the phone's `validPacket` relay gate).
    fn relay_public_packet(&self, packet: &Packet, via: &Route) {
        if packet.ttl == 0 {
            return;
        }
        let mut relayed = packet.clone();
        relayed.ttl = packet.ttl - 1;
        for route in self.routes.values() {
            let same_central = matches!(&route.central, Some(a) if via.central.as_ref() == Some(a));
            let same_peripheral =
                matches!(&route.peripheral, Some(a) if via.peripheral.as_ref() == Some(a));
            if same_central || same_peripheral {
                continue; // don't send back on the arrival channel
            }
            self.send_frame(route, &relayed);
        }
    }

    async fn handle_noise_handshake(&mut self, sender: &str, packet: Packet, via: Route) {
        if packet.recipient_id != Some(self.identity.peer_id_bytes()) {
            return; // not addressed to us (relay case comes later)
        }

        // Existing session: feed the message. On failure, a 32-byte payload is
        // the peer's fresh initiator message racing our own attempt; demote to
        // responder, mirroring the Android responder-candidate path.
        let feed = match self.sessions.get_mut(sender) {
            Some(session) => match session.process_handshake_message(&packet.payload) {
                Ok(Some(response)) => Feed::Response(response),
                Ok(None) => Feed::Established,
                Err(_) if packet.payload.len() == 32 => {
                    self.sessions.remove(sender);
                    self.initiate_responder(sender, &packet.payload, &via).await
                }
                Err(_) => Feed::Failed,
            },
            None => self.initiate_responder(sender, &packet.payload, &via).await,
        };

        match feed {
            Feed::Response(response) => {
                if !response.is_empty() {
                    let route = self
                        .routes
                        .get(sender)
                        .cloned()
                        .unwrap_or_else(|| via.clone());
                    self.send_handshake_packet(&route, sender, response).await;
                }
                self.maybe_established(sender);
            }
            Feed::Established => self.maybe_established(sender),
            Feed::Failed => self.info(format!("handshake with {sender} failed")),
        }
    }

    /// Create a responder session and process the initiator's first message.
    async fn initiate_responder(&mut self, sender: &str, payload: &[u8], via: &Route) -> Feed {
        let mut session =
            match NoiseSession::new(sender, false, self.identity.noise_private_key, now_ms()) {
                Ok(session) => session,
                Err(e) => {
                    self.info(format!("responder session for {sender}: {e}"));
                    return Feed::Failed;
                }
            };
        match session.process_handshake_message(payload) {
            Ok(Some(response)) => {
                self.peerstate_sent.remove(sender);
                self.sessions.insert(sender.to_string(), session);
                self.routes
                    .entry(sender.to_string())
                    .or_insert_with(|| via.clone());
                Feed::Response(response)
            }
            Ok(None) => {
                self.peerstate_sent.remove(sender);
                self.sessions.insert(sender.to_string(), session);
                self.routes
                    .entry(sender.to_string())
                    .or_insert_with(|| via.clone());
                Feed::Established
            }
            Err(e) => {
                self.info(format!("handshake process from {sender}: {e}"));
                Feed::Failed
            }
        }
    }

    fn maybe_established(&mut self, peer: &str) {
        let established = self
            .sessions
            .get(peer)
            .map(|s| s.is_established())
            .unwrap_or(false);
        if !established {
            return;
        }
        self.decrypt_failures.remove(peer);
        let _ = self.events.try_send(LinkEvent::SessionEstablished {
            peer_id: peer.to_string(),
        });
        // Mirror the phone client: authenticate the session by sending our
        // PeerState as the first encrypted message. This unlocks the peer's
        // UI/session so it can reply without waiting for an inbound message.
        if !self.peerstate_sent.contains(peer) {
            self.send_peer_state(peer);
        }
        let pending = self.pending.remove(peer).unwrap_or_default();
        for (message_id, content) in pending {
            self.deliver_private(peer, &message_id, &content);
        }
    }

    async fn handle_noise_encrypted(&mut self, sender: &str, packet: Packet) {
        let debug = std::env::var_os("BITCHATD_DEBUG_HEX").is_some();
        if packet.recipient_id != Some(self.identity.peer_id_bytes()) {
            if debug {
                eprintln!(
                    "[debug-hex] 0x11 dropped: recipient mismatch {:?} != ours",
                    packet.recipient_id.map(|r| to_hex(&r))
                );
            }
            return;
        }

        let plaintext = {
            let Some(session) = self.sessions.get_mut(sender) else {
                // Our session state can vanish during link churn (peer
                // reconnects over the other role). Ask for a fresh handshake
                // instead of staying deaf: the phone answers a 0x10 even when
                // it still holds a session, and re-establishes with new keys.
                self.info(format!(
                    "NOISE_ENCRYPTED from {sender} without session; requesting re-handshake"
                ));
                self.initiate_handshake(sender).await;
                return;
            };
            let out = session.decrypt(&packet.payload);
            if debug {
                eprintln!(
                    "[debug-hex] 0x11 decrypt from {sender}: {}",
                    if out.is_ok() { "ok" } else { "FAILED" }
                );
            }
            out
        };

        match plaintext {
            Ok(data) => {
                self.decrypt_failures.remove(sender);
                let Some(payload) = NoisePayload::decode(&data) else {
                    self.info(format!("undecodable NoisePayload from {sender}"));
                    return;
                };
                if debug {
                    eprintln!(
                        "[debug-hex] 0x11 payload type={:?} len={}",
                        payload.payload_type,
                        payload.data.len()
                    );
                }
                match payload.payload_type {
                    NoisePayloadType::PrivateMessage => {
                        match PrivateMessagePacket::decode(&payload.data) {
                            Some(message) => {
                                // Phone-parity behaviour: acknowledge receipt so
                                // the sender's UI can mark the message delivered.
                                self.deliver_delivered(sender, &message.message_id);
                                let _ = self.events.try_send(LinkEvent::PrivateMessage {
                                    peer_id: sender.to_string(),
                                    message_id: message.message_id,
                                    content: message.content,
                                });
                            }
                            None => self.info(format!("undecodable private message from {sender}")),
                        }
                    }
                    NoisePayloadType::Delivered => {
                        let message_id = String::from_utf8_lossy(&payload.data).into_owned();
                        let _ = self.events.try_send(LinkEvent::Delivered {
                            peer_id: sender.to_string(),
                            message_id,
                        });
                    }
                    NoisePayloadType::PeerState => {
                        match AuthenticatedPeerState::decode(&payload.data) {
                            Some(state) => {
                                // Echo our own state for this generation if we
                                // have not sent it yet (phone-parity behaviour).
                                if !self.peerstate_sent.contains(sender) {
                                    self.send_peer_state(sender);
                                }
                                let _ = self.events.try_send(LinkEvent::PeerState {
                                    peer_id: sender.to_string(),
                                    capabilities: state.capabilities.0,
                                    signing_public_key: to_hex(&state.signing_public_key),
                                });
                            }
                            None => {
                                self.info(format!("undecodable peer state from {sender}"))
                            }
                        }
                    }
                    other => self.info(format!("noise payload {other:?} from {sender} ignored")),
                }
            }
            Err(e) => {
                let failures = self.decrypt_failures.entry(sender.to_string()).or_default();
                *failures += 1;
                let count = *failures;
                self.info(format!(
                    "decrypt from {sender} failed ({e}); {count} consecutive"
                ));
                if count >= MAX_DECRYPT_FAILURES {
                    self.decrypt_failures.remove(sender);
                    self.sessions.remove(sender);
                    self.info(format!(
                        "session with {sender} dropped after decrypt failures"
                    ));
                    // Re-handshake so the next peer message has a live session.
                    self.initiate_handshake(sender).await;
                }
            }
        }
    }

    async fn send_private_message(&mut self, peer_id: &str, content: &str) {
        let message_id = uuid::Uuid::new_v4().to_string();
        if self
            .sessions
            .get(peer_id)
            .map(|s| s.is_established())
            .unwrap_or(false)
        {
            self.deliver_private(peer_id, &message_id, content);
            return;
        }
        self.pending
            .entry(peer_id.to_string())
            .or_default()
            .push((message_id, content.to_string()));
        self.initiate_handshake(peer_id).await;
    }

    /// Encrypt and send a private chat message to an established peer.
    fn deliver_private(&mut self, peer_id: &str, message_id: &str, content: &str) {
        let Some(route) = self.routes.get(peer_id).cloned() else {
            self.info(format!("no route to {peer_id}"));
            return;
        };
        if route.is_empty() {
            self.info(format!("no route to {peer_id}"));
            return;
        }
        let Some(session) = self.sessions.get_mut(peer_id) else {
            self.info(format!("no session with {peer_id}"));
            return;
        };
        let packet_tlv = match PrivateMessagePacket::new(message_id, content).encode() {
            Some(tlv) => tlv,
            None => {
                self.info("private message too long to encode");
                return;
            }
        };
        let envelope = NoisePayload::new(NoisePayloadType::PrivateMessage, packet_tlv).encode();
        let ciphertext = match session.encrypt(&envelope) {
            Ok(data) => data,
            Err(e) => {
                self.info(format!("encrypt for {peer_id} failed: {e}"));
                return;
            }
        };
        let mut packet = Packet::new(
            MessageType::NoiseEncrypted as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            ciphertext,
        );
        packet.recipient_id = peer_bytes(peer_id);
        packet.timestamp = now_ms();
        self.sign_and_send(&route, packet);
    }

    /// Encrypt and send our AuthenticatedPeerState (Noise payload 0x21) to a
    /// peer with an established session, mirroring the phone client's
    /// post-authentication announcement. Marks the generation as sent.
    fn send_peer_state(&mut self, peer_id: &str) {
        let Some(route) = self.routes.get(peer_id).cloned() else {
            return;
        };
        if route.is_empty() {
            return;
        }
        let Some(session) = self.sessions.get_mut(peer_id) else {
            return;
        };
        if !session.is_established() {
            return;
        }
        let state = AuthenticatedPeerState::new(
            PeerCapabilities(ADVERTISED_CAPABILITIES),
            self.identity.signing.public_key(),
        );
        let envelope = NoisePayload::new(NoisePayloadType::PeerState, state.encode()).encode();
        let ciphertext = match session.encrypt(&envelope) {
            Ok(ciphertext) => ciphertext,
            Err(e) => {
                self.info(format!("peer state encrypt for {peer_id}: {e}"));
                return;
            }
        };
        let mut packet = Packet::new(
            MessageType::NoiseEncrypted as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            ciphertext,
        );
        packet.recipient_id = peer_bytes(peer_id);
        packet.timestamp = now_ms();
        self.sign_and_send(&route, packet);
        self.peerstate_sent.insert(peer_id.to_string());
        self.info(format!("[peerstate] sent to {peer_id}"));
        let _ = self.events.try_send(LinkEvent::PeerStateSent {
            peer_id: peer_id.to_string(),
        });
    }

    /// Encrypt and send a Delivered ack for a received private message,
    /// mirroring the phone client's automatic acknowledgement.
    fn deliver_delivered(&mut self, peer_id: &str, message_id: &str) {
        let Some(route) = self.routes.get(peer_id).cloned() else {
            return;
        };
        if route.is_empty() {
            return;
        }
        let Some(session) = self.sessions.get_mut(peer_id) else {
            return;
        };
        if !session.is_established() {
            return;
        }
        let envelope =
            NoisePayload::new(NoisePayloadType::Delivered, message_id.as_bytes().to_vec())
                .encode();
        let ciphertext = match session.encrypt(&envelope) {
            Ok(data) => data,
            Err(e) => {
                self.info(format!("delivered ack encrypt for {peer_id} failed: {e}"));
                return;
            }
        };
        let mut packet = Packet::new(
            MessageType::NoiseEncrypted as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            ciphertext,
        );
        packet.recipient_id = peer_bytes(peer_id);
        packet.timestamp = now_ms();
        self.sign_and_send(&route, packet);
    }

    async fn initiate_handshake(&mut self, peer_id: &str) {
        if self.sessions.contains_key(peer_id) {
            return; // in flight or established
        }
        let Some(route) = self.routes.get(peer_id).cloned() else {
            self.info(format!("cannot handshake {peer_id}: no route"));
            return;
        };
        if route.is_empty() {
            self.info(format!("cannot handshake {peer_id}: no route"));
            return;
        }
        let mut session =
            match NoiseSession::new(peer_id, true, self.identity.noise_private_key, now_ms()) {
                Ok(session) => session,
                Err(e) => {
                    self.info(format!("initiator session for {peer_id}: {e}"));
                    return;
                }
            };
        match session.start_handshake() {
            Ok(message) => {
                self.peerstate_sent.remove(peer_id);
                self.sessions.insert(peer_id.to_string(), session);
                self.send_handshake_packet(&route, peer_id, message).await;
            }
            Err(e) => self.info(format!("handshake start for {peer_id}: {e}")),
        }
    }

    async fn send_handshake_packet(&self, route: &Route, peer_id: &str, payload: Vec<u8>) {
        let mut packet = Packet::new(
            MessageType::NoiseHandshake as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            payload,
        );
        packet.recipient_id = peer_bytes(peer_id);
        packet.timestamp = now_ms();
        self.sign_and_send(route, packet);
    }

    async fn send_announce_to(&self, route: &Route) {
        let announce = IdentityAnnouncement {
            nickname: self.nickname.clone(),
            noise_public_key: self.identity.noise_public_key.to_vec(),
            signing_public_key: self.identity.signing.public_key().to_vec(),
            capabilities: Some(bitchat_wire::announce::PeerCapabilities(
                ADVERTISED_CAPABILITIES,
            )),
            unknown_tlvs: Vec::new(),
        };
        let payload = announce.encode().unwrap_or_default();
        let mut packet = Packet::new(
            MessageType::Announce as u8,
            MAX_TTL,
            self.identity.peer_id_bytes(),
            payload,
        );
        packet.timestamp = now_ms();
        self.sign_and_send(route, packet);
    }

    fn sign_and_send(&self, route: &Route, mut packet: Packet) {
        packet.signature = self.identity.signing.sign_packet(&packet);
        if std::env::var_os("BITCHATD_DEBUG_HEX").is_some() {
            if let Some(frame) = bitchat_wire::packet::encode(&packet) {
                eprintln!(
                    "[debug-hex] tx type={:#04x} len={} {}",
                    packet.packet_type,
                    frame.len(),
                    to_hex(&frame)
                );
            }
        }
        self.send_frame(route, &packet);
    }

    /// Write an already-signed packet to every channel of a route. Relays and
    /// sync responses keep the originator's signature untouched.
    fn send_frame(&self, route: &Route, packet: &Packet) {
        if let Some(peripheral) = &route.central {
            let _ = self.central_commands.try_send(CentralCommand::Send {
                id: peripheral.clone(),
                packet: packet.clone(),
            });
        }
        #[cfg(windows)]
        if let (Some(peripheral), Some(handle)) = (&route.peripheral, &self.peripheral) {
            let _ = handle.send(PeripheralCommand::Send {
                session: peripheral.clone(),
                packet: packet.clone(),
            });
        }
    }
}


/// Start the GATT server on Windows; a never-fed event stream elsewhere.
#[cfg(windows)]
fn spawn_peripheral(
    peer_id: [u8; 8],
) -> (Option<PeripheralHandle>, mpsc::Receiver<PeripheralEvent>) {
    let (handle, events) = bitchat_ble::peripheral::start(peer_id);
    (Some(handle), events)
}

/// Start the GATT server on Windows; a never-fed event stream elsewhere.
#[cfg(not(windows))]
fn spawn_peripheral(
    _peer_id: [u8; 8],
) -> (Option<PeripheralHandle>, mpsc::Receiver<PeripheralEvent>) {
    (None, mpsc::channel(1).1)
}

/// Placeholder event type for non-Windows builds (the GATT server is WinRT).
#[cfg(not(windows))]
#[derive(Debug)]
enum PeripheralEvent {
    _Never,
}

/// Placeholder handle for non-Windows builds.
#[cfg(not(windows))]
struct PeripheralHandle;

#[cfg(not(windows))]
impl Actor {
    async fn handle_peripheral(&mut self, _event: PeripheralEvent) {}
}

/// Capabilities we truthfully advertise (no media transfer support yet).
const ADVERTISED_CAPABILITIES: u64 = 0;

/// Result of feeding a handshake message into a (possibly new) session.
enum Feed {
    Response(Vec<u8>),
    Established,
    Failed,
}

fn peer_bytes(peer_id: &str) -> Option<[u8; 8]> {
    if peer_id.len() != 16 {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, chunk) in peer_id.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}
