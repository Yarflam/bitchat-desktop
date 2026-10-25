//! Mesh link layer: drives the BLE central, announces the node, establishes
//! Noise sessions, and exchanges private messages — byte-compatible with the
//! Android client's mesh orchestration (`MeshCore`, `MessageHandler`).

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use bitchat_ble::central::{CentralCommand, CentralEventOut, CentralHandle};
#[cfg(windows)]
use bitchat_ble::peripheral::{PeripheralCommand, PeripheralEvent, PeripheralHandle};
use bitchat_ble::PeripheralId;
use bitchat_wire::announce::IdentityAnnouncement;
use bitchat_wire::identity::verify_packet;
use bitchat_wire::noise::{derive_peer_id, NoiseSession};
use bitchat_wire::noise_payload::{NoisePayload, NoisePayloadType, PrivateMessagePacket};
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
    PeerDisconnected {
        peer_id: String,
    },
    Info(String),
}

/// Commands accepted by a running [`MeshLink`].
#[derive(Debug)]
pub enum LinkCommand {
    SendPrivateMessage { peer_id: String, content: String },
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
        let (peripheral, peripheral_events) = spawn_peripheral();

        let mut actor = Actor {
            identity,
            nickname: nickname.to_string(),
            central_events,
            central_commands,
            peripheral,
            peripheral_events,
            commands: command_rx,
            events: event_tx,
            sessions: HashMap::new(),
            routes: HashMap::new(),
            peers: HashMap::new(),
            verified: HashMap::new(),
            pending: HashMap::new(),
            decrypt_failures: HashMap::new(),
            connecting: HashSet::new(),
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
    connecting: HashSet<PeripheralId>,
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
                    Some(LinkCommand::Shutdown) | None => break,
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
            MessageType::Announce => self.handle_announce(sender.to_string(), &packet).await,
            MessageType::NoiseHandshake => self.handle_noise_handshake(sender, packet, via).await,
            MessageType::NoiseEncrypted => self.handle_noise_encrypted(sender, packet).await,
            other => self.info(format!(
                "ignoring {other:?} from {sender} (not implemented yet)"
            )),
        }
    }

    async fn handle_announce(&mut self, sender: String, packet: &Packet) {
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
            .map(|_| verify_packet(packet, &signing_key))
            .unwrap_or(false);
        self.peers.insert(sender.clone(), announce.clone());
        self.verified.insert(sender.clone(), is_verified);
        let _ = self.events.try_send(LinkEvent::PeerAnnounced {
            peer_id: sender.clone(),
            nickname: announce.nickname.clone(),
            verified: is_verified,
            capabilities: announce.capabilities.map(|c| c.0 as u16),
        });
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
                self.sessions.insert(sender.to_string(), session);
                self.routes
                    .entry(sender.to_string())
                    .or_insert_with(|| via.clone());
                Feed::Response(response)
            }
            Ok(None) => {
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
                self.info(format!(
                    "NOISE_ENCRYPTED from {sender} without session; ignored"
                ));
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
                packet,
            });
        }
    }
}

/// Start the GATT server on Windows; a never-fed event stream elsewhere.
#[cfg(windows)]
fn spawn_peripheral() -> (Option<PeripheralHandle>, mpsc::Receiver<PeripheralEvent>) {
    let (handle, events) = bitchat_ble::peripheral::start();
    (Some(handle), events)
}

/// Start the GATT server on Windows; a never-fed event stream elsewhere.
#[cfg(not(windows))]
fn spawn_peripheral() -> (Option<PeripheralHandle>, mpsc::Receiver<PeripheralEvent>) {
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
