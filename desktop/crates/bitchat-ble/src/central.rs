//! btleplug-based GATT central node.
//!
//! Discovers phones advertising the BitChat service, connects, subscribes to
//! the mesh characteristic, and exchanges one-frame-per-notification packets.
//! Works on Linux, Windows, and macOS in the central role (see
//! `docs/desktop-node-spec.md` §5.1 for platform caveats).
//!
//! MTU note: btleplug exposes no MTU negotiation API. Android peers request
//! 517 when THEY are the client; when our node is the central, the negotiated
//! MTU is whatever the platform stack picks. We therefore budget for the
//! Android-default ATT payload and fail closed on oversized frames — the
//! protocol fragmentation layer guarantees frames fit 512+overhead, which any
//! negotiated MTU >= 515 satisfies. Mesh Lab validates this against real
//! phones.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bitchat_wire::packet::Packet;
use btleplug::api::{
    Central, CentralEvent, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral, PeripheralId};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::frame::{
    advertised_peer_id, decode_frame, encode_frame, peer_id_from_sender, CHARACTERISTIC_UUID,
    DEFAULT_ATT_MTU, SERVICE_UUID,
};

/// How long to wait for service discovery before giving up on a connection.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the transport connection itself. Slightly above the
/// platform default: an existing system-level GATT session to the peer (e.g.
/// Windows auto-connecting to a paired device) makes the first attempt slow.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);
/// How long to wait for the adapter list at startup.
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum CentralError {
    #[error("no Bluetooth adapter available")]
    NoAdapter,
    #[error("Bluetooth operation failed: {0}")]
    Ble(#[from] btleplug::Error),
    #[error(transparent)]
    Frame(#[from] crate::frame::FrameError),
    #[error("BitChat GATT service not found on peer")]
    ServiceNotFound,
    #[error("mesh characteristic not found on peer")]
    CharacteristicNotFound,
}

/// A peer seen (or connected) by the central.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: PeripheralId,
    pub address: String,
    /// Wire peer ID from the advertisement service data, when advertised.
    pub advertised_peer_id: Option<[u8; 8]>,
    pub rssi: Option<i16>,
}

/// Events emitted by the central node.
#[derive(Debug)]
pub enum CentralEventOut {
    /// A new peer was discovered or its advertisement updated.
    Discovered(PeerInfo),
    /// A connection is up and notifications are subscribed.
    Connected {
        id: PeripheralId,
        info: PeerInfo,
    },
    Disconnected {
        id: PeripheralId,
    },
    /// One decoded bitchat frame arrived from a peer.
    Packet {
        id: PeripheralId,
        peer_id: String,
        packet: Packet,
    },
    /// The radio disappeared or the adapter failed; the caller should restart.
    AdapterLost(String),
}

/// Commands accepted by the central node.
#[derive(Debug)]
pub enum CentralCommand {
    Connect(PeripheralId),
    /// Connect directly to a known MAC address, without scanning — for
    /// bonded peers whose advertisement is unreachable or disabled.
    ConnectByAddress([u8; 6]),
    Disconnect(PeripheralId),
    Send {
        id: PeripheralId,
        packet: Packet,
    },
    Shutdown,
}

fn peripheral_id_from_address(address: [u8; 6]) -> Option<PeripheralId> {
    #[cfg(windows)]
    {
        Some(PeripheralId::from(btleplug::api::BDAddr::from(address)))
    }
    #[cfg(not(windows))]
    {
        let _ = address;
        None
    }
}

/// Handle for driving a running [`CentralNode`].
pub struct CentralHandle {
    pub events: mpsc::Receiver<CentralEventOut>,
    commands: mpsc::Sender<CentralCommand>,
}

impl CentralHandle {
    pub async fn send(
        &self,
        command: CentralCommand,
    ) -> Result<(), mpsc::error::SendError<CentralCommand>> {
        self.commands.send(command).await
    }
}

struct Connection {
    peripheral: Peripheral,
    /// Frame sink for writes: the negotiated characteristic.
    characteristic: btleplug::api::Characteristic,
}

/// Start the central node: scans, connects, and routes frames.
///
/// Returns a [`CentralHandle`] for commands and a stream of events. The
/// returned task runs until [`CentralCommand::Shutdown`] or adapter failure.
pub async fn run_central(event_buffer: usize) -> Result<CentralHandle, CentralError> {
    let manager = Manager::new().await.map_err(CentralError::Ble)?;
    let adapters = tokio::time::timeout(ADAPTER_TIMEOUT, manager.adapters())
        .await
        .map_err(|_| CentralError::NoAdapter)?
        .map_err(CentralError::Ble)?;
    let adapter = adapters.into_iter().next().ok_or(CentralError::NoAdapter)?;

    let (event_tx, event_rx) = mpsc::channel(event_buffer);
    let (cmd_tx, cmd_rx) = mpsc::channel(64);

    tokio::spawn(async move {
        if let Err(e) = central_loop(adapter, event_tx.clone(), cmd_rx).await {
            let _ = event_tx
                .send(CentralEventOut::AdapterLost(e.to_string()))
                .await;
        }
    });

    Ok(CentralHandle {
        events: event_rx,
        commands: cmd_tx,
    })
}

async fn central_loop(
    adapter: btleplug::platform::Adapter,
    events: mpsc::Sender<CentralEventOut>,
    mut commands: mpsc::Receiver<CentralCommand>,
) -> Result<(), CentralError> {
    let mut connections: HashMap<PeripheralId, Arc<Connection>> = HashMap::new();
    let mut known: HashMap<PeripheralId, PeerInfo> = HashMap::new();

    // Scan unfiltered and filter in user space: host-side service filtering
    // silently drops advertisements on some platforms (notably Windows),
    // while the BitChat service data remains visible in the raw properties.
    adapter.start_scan(ScanFilter::default()).await?;

    let mut central_events = adapter.events().await.map_err(CentralError::Ble)?;

    loop {
        tokio::select! {
            Some(event) = central_events.next() => {
                match event {
                    CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {
                        if let Ok(peripheral) = adapter.peripheral(&id).await {
                            if let Ok(Some(properties)) = peripheral.properties().await {
                                let is_bitchat = properties.services.contains(&SERVICE_UUID)
                                    || advertised_peer_id(&properties.service_data).is_some();
                                if !is_bitchat {
                                    continue;
                                }
                                let info = PeerInfo {
                                    id: id.clone(),
                                    address: properties.address.to_string(),
                                    advertised_peer_id: advertised_peer_id(&properties.service_data),
                                    rssi: properties.rssi,
                                };
                                known.insert(id.clone(), info.clone());
                                let _ = events.send(CentralEventOut::Discovered(info)).await;
                            }
                        }
                    }
                    CentralEvent::DeviceDisconnected(id) => {
                        connections.remove(&id);
                        let _ = events.send(CentralEventOut::Disconnected { id }).await;
                    }
                    _ => {}
                }
            }
            Some(command) = commands.recv() => {
                match command {
                    CentralCommand::Shutdown => break,
                    CentralCommand::Connect(id) => {
                        if connections.contains_key(&id) {
                            continue;
                        }
                        connect_and_track(&adapter, &mut connections, &mut known, &id, events.clone()).await;
                    }
                    CentralCommand::ConnectByAddress(address) => {
                        match peripheral_id_from_address(address) {
                            Some(id) if !connections.contains_key(&id) => {
                                connect_and_track(&adapter, &mut connections, &mut known, &id, events.clone()).await;
                            }
                            _ => {
                                let _ = events.send(CentralEventOut::AdapterLost(format!(
                                    "connect-by-address unsupported for {address:02x?}"
                                ))).await;
                            }
                        }
                    }
                    CentralCommand::Disconnect(id) => {
                        if let Some(connection) = connections.remove(&id) {
                            let _ = connection.peripheral.disconnect().await;
                        }
                    }
                    CentralCommand::Send { id, packet } => {
                        if let Some(connection) = connections.get(&id) {
                            match encode_frame(&packet, DEFAULT_ATT_MTU) {
                                Ok(frame) => {
                                    if let Err(e) = connection
                                        .peripheral
                                        .write(&connection.characteristic, &frame, WriteType::WithoutResponse)
                                        .await
                                    {
                                        let _ = events.send(CentralEventOut::AdapterLost(format!(
                                            "write failed for {id}: {e}"
                                        ))).await;
                                    }
                                }
                                Err(e) => {
                                    let _ = events.send(CentralEventOut::AdapterLost(format!(
                                        "frame encoding failed for {id}: {e}"
                                    ))).await;
                                }
                            }
                        }
                    }
                }
            }
            else => break,
        }
    }

    adapter.stop_scan().await.ok();
    for (_, connection) in connections.drain() {
        let _ = connection.peripheral.disconnect().await;
    }
    Ok(())
}

async fn connect_and_track(
    adapter: &btleplug::platform::Adapter,
    connections: &mut HashMap<PeripheralId, Arc<Connection>>,
    known: &mut HashMap<PeripheralId, PeerInfo>,
    id: &PeripheralId,
    events: mpsc::Sender<CentralEventOut>,
) {
    let peripheral = match adapter.peripheral(id).await {
        Ok(p) => p,
        Err(e) => {
            let _ = events
                .send(CentralEventOut::AdapterLost(format!(
                    "lookup failed for {id}: {e}"
                )))
                .await;
            return;
        }
    };
    match connect_peer(&peripheral).await {
        Ok(connection) => {
            connections.insert(id.clone(), Arc::new(connection));
            let info = known.get(id).cloned().unwrap_or(PeerInfo {
                id: id.clone(),
                address: peripheral.address().to_string(),
                advertised_peer_id: None,
                rssi: None,
            });
            let _ = events
                .send(CentralEventOut::Connected {
                    id: id.clone(),
                    info,
                })
                .await;
            spawn_notification_loop(peripheral, id.clone(), events);
        }
        Err(e) => {
            let _ = events
                .send(CentralEventOut::AdapterLost(format!(
                    "connect failed for {id}: {e}"
                )))
                .await;
        }
    }
}

async fn connect_peer(peripheral: &Peripheral) -> Result<Connection, CentralError> {
    peripheral
        .connect_with_timeout(CONNECT_TIMEOUT)
        .await
        .map_err(CentralError::Ble)?;
    let result = async {
        peripheral
            .discover_services_with_timeout(DISCOVERY_TIMEOUT)
            .await?;
        for service in peripheral.services() {
            if service.uuid != SERVICE_UUID {
                continue;
            }
            for characteristic in &service.characteristics {
                if characteristic.uuid == CHARACTERISTIC_UUID
                    && characteristic
                        .properties
                        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
                {
                    peripheral.subscribe(characteristic).await?;
                    return Ok(Connection {
                        peripheral: peripheral.clone(),
                        characteristic: characteristic.clone(),
                    });
                }
            }
        }
        Err(CentralError::CharacteristicNotFound)
    }
    .await;

    if result.is_err() {
        let _ = peripheral.disconnect().await;
    }
    result
}

fn spawn_notification_loop(
    peripheral: Peripheral,
    id: PeripheralId,
    events: mpsc::Sender<CentralEventOut>,
) {
    tokio::spawn(async move {
        let mut notifications = match peripheral.notifications().await {
            Ok(n) => n,
            Err(_) => return,
        };
        while let Some(notification) = notifications.next().await {
            if notification.uuid != CHARACTERISTIC_UUID {
                continue;
            }
            if let Some(packet) = decode_frame(&notification.value) {
                let peer_id = peer_id_from_sender(&packet.sender_id);
                if events
                    .send(CentralEventOut::Packet {
                        id: id.clone(),
                        peer_id,
                        packet,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}
