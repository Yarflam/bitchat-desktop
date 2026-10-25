//! WinRT GATT server (peripheral role): advertises the BitChat service,
//! receives mesh packets as characteristic writes, and pushes packets to
//! subscribed centrals as notifications.
//!
//! Architecture: a dedicated std thread owns the WinRT objects and drains
//! commands; system-invoked event handlers only push events into a channel.

#[cfg(windows)]
mod imp {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use tokio::sync::mpsc;

    use windows::core::GUID;
    use windows::Devices::Bluetooth::GenericAttributeProfile::*;
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};

    use bitchat_wire::packet::Packet;

    use crate::frame::{decode_frame, encode_frame, DEFAULT_ATT_MTU};

    /// BitChat mesh service UUID.
    pub const SERVICE_UUID: GUID = GUID::from_u128(0xF47B5E2D_4A9E_4C5A_9B3F_8E1D2C3A4B5C);
    /// Mesh data characteristic UUID (write + notify).
    pub const CHARACTERISTIC_UUID: GUID = GUID::from_u128(0xA1B2C3D4_E5F6_4A5B_8C9D_0E1F2A3B4C5D);

    /// Events emitted by the peripheral node.
    #[derive(Debug)]
    #[allow(clippy::large_enum_variant)] // Packet dominates; events are rare.
    pub enum PeripheralEvent {
        /// Advertising is on and the service is discoverable.
        AdvertisingStarted,
        AdvertisingFailed(String),
        /// A central enabled notifications on the mesh characteristic.
        Subscribed {
            session: String,
        },
        Unsubscribed {
            session: String,
        },
        /// A decoded mesh packet written by a central.
        Packet {
            session: String,
            peer_id: String,
            packet: Packet,
        },
        Info(String),
    }

    /// Commands for the peripheral node.
    #[derive(Debug)]
    #[allow(clippy::large_enum_variant)] // Packet dominates; commands are rare.
    pub enum PeripheralCommand {
        /// Send one packet to one connected central.
        Send {
            session: String,
            packet: Packet,
        },
        /// Broadcast one packet to all subscribed centrals.
        SendAll(Packet),
        Shutdown,
    }

    /// Async handle for a running peripheral node (Windows only).
    pub struct PeripheralHandle {
        commands: std::sync::mpsc::Sender<PeripheralCommand>,
    }

    impl PeripheralHandle {
        #[allow(clippy::result_large_err)] // failed commands are returned for reuse
        pub fn send(&self, command: PeripheralCommand) -> Result<(), PeripheralCommand> {
            self.commands.send(command).map_err(|e| e.0)
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn session_key(session: &GattSession) -> Option<String> {
        session.DeviceId().ok()?.Id().ok().map(|id| id.to_string())
    }

    fn make_buffer(data: &[u8]) -> windows::core::Result<IBuffer> {
        let writer = DataWriter::new()?;
        writer.WriteBytes(data)?;
        let buffer = writer.DetachBuffer()?;
        writer.Close()?;
        Ok(buffer)
    }

    /// Start the GATT server and begin advertising. Returns the command
    /// handle plus the event stream.
    pub fn start() -> (PeripheralHandle, mpsc::Receiver<PeripheralEvent>) {
        let (event_tx, event_rx) = std::sync::mpsc::channel::<PeripheralEvent>();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<PeripheralCommand>();

        thread::spawn(move || {
            if let Err(message) = run(event_tx.clone(), cmd_rx) {
                let _ = event_tx.send(PeripheralEvent::AdvertisingFailed(message));
            }
        });

        // Bridge std events into tokio.
        let (tokio_tx, tokio_rx) = mpsc::channel(256);
        let shared_rx = Arc::new(Mutex::new(event_rx));
        tokio::spawn(async move {
            loop {
                let shared_rx = shared_rx.clone();
                let recv =
                    tokio::task::spawn_blocking(move || shared_rx.lock().unwrap().recv()).await;
                match recv {
                    Ok(Ok(event)) => {
                        if tokio_tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });

        (PeripheralHandle { commands: cmd_tx }, tokio_rx)
    }

    fn run(
        event_tx: std::sync::mpsc::Sender<PeripheralEvent>,
        cmd_rx: std::sync::mpsc::Receiver<PeripheralCommand>,
    ) -> Result<(), String> {
        let create = GattServiceProvider::CreateAsync(SERVICE_UUID)
            .and_then(|op| op.get())
            .map_err(|e| format!("create service provider: {e}"))?;
        let provider = create.ServiceProvider().map_err(|e| e.to_string())?;
        let service = provider.Service().map_err(|e| e.to_string())?;

        let params = GattLocalCharacteristicParameters::new().map_err(|e| e.to_string())?;
        params
            .SetCharacteristicProperties(
                GattCharacteristicProperties::Write
                    | GattCharacteristicProperties::WriteWithoutResponse
                    | GattCharacteristicProperties::Notify,
            )
            .map_err(|e| e.to_string())?;
        params
            .SetWriteProtectionLevel(GattProtectionLevel::Plain)
            .map_err(|e| e.to_string())?;
        let characteristic = service
            .CreateCharacteristicAsync(CHARACTERISTIC_UUID, &params)
            .and_then(|op| op.get())
            .and_then(|result| result.Characteristic())
            .map_err(|e| format!("create characteristic: {e}"))?;

        let clients: Arc<Mutex<BTreeMap<String, GattSubscribedClient>>> =
            Arc::new(Mutex::new(BTreeMap::new()));

        // Incoming writes: one mesh frame per write.
        {
            let event_tx = event_tx.clone();
            characteristic
                .WriteRequested(&TypedEventHandler::new(
                    move |_: windows::core::Ref<'_, GattLocalCharacteristic>,
                          args: windows::core::Ref<'_, GattWriteRequestedEventArgs>| {
                        if let Err(e) = handle_write(args, &event_tx) {
                            let _ = event_tx.send(PeripheralEvent::Info(format!(
                                "write handling failed: {e}"
                            )));
                        }
                        Ok(())
                    },
                ))
                .map_err(|e| format!("register write handler: {e}"))?;
        }

        // Subscription tracking = per-peer connection signal.
        {
            let event_tx = event_tx.clone();
            let clients_for_handler = clients.clone();
            let characteristic_for_handler = characteristic.clone();
            characteristic
                .SubscribedClientsChanged(&TypedEventHandler::new(
                    move |_: windows::core::Ref<'_, GattLocalCharacteristic>, _| {
                        let current = characteristic_for_handler
                            .SubscribedClients()
                            .map(|list| {
                                (0..list.Size().unwrap_or(0))
                                    .filter_map(|i| list.GetAt(i).ok())
                                    .filter_map(|client| {
                                        session_key(&client.Session().ok()?)
                                            .map(|key| (key, client))
                                    })
                                    .collect::<BTreeMap<_, _>>()
                            })
                            .unwrap_or_default();
                        let mut registry = clients_for_handler.lock().unwrap();
                        let before: BTreeSet<String> = registry.keys().cloned().collect();
                        let now: BTreeSet<String> = current.keys().cloned().collect();
                        for key in now.difference(&before) {
                            let _ = event_tx.send(PeripheralEvent::Subscribed {
                                session: key.clone(),
                            });
                        }
                        for key in before.difference(&now) {
                            registry.remove(key);
                            let _ = event_tx.send(PeripheralEvent::Unsubscribed {
                                session: key.clone(),
                            });
                        }
                        for (key, client) in current {
                            registry.entry(key).or_insert(client);
                        }
                        Ok(())
                    },
                ))
                .map_err(|e| format!("register subscribe handler: {e}"))?;
        }

        // Keep provider alive: held by this thread until the command loop ends.
        let adv_params =
            GattServiceProviderAdvertisingParameters::new().map_err(|e| e.to_string())?;
        adv_params
            .SetIsDiscoverable(true)
            .map_err(|e| e.to_string())?;
        adv_params
            .SetIsConnectable(true)
            .map_err(|e| e.to_string())?;
        provider
            .StartAdvertisingWithParameters(&adv_params)
            .map_err(|e| format!("start advertising: {e}"))?;
        let _ = event_tx.send(PeripheralEvent::AdvertisingStarted);

        // Command loop: this thread owns the WinRT objects for their lifetime.
        while let Ok(command) = cmd_rx.recv() {
            match command {
                PeripheralCommand::Send { session, packet } => {
                    let frame = match encode_frame(&packet, DEFAULT_ATT_MTU) {
                        Ok(frame) => frame,
                        Err(e) => {
                            let _ =
                                event_tx.send(PeripheralEvent::Info(format!("encode failed: {e}")));
                            continue;
                        }
                    };
                    let client = clients.lock().unwrap().get(&session).cloned();
                    if let Some(client) = client {
                        match make_buffer(&frame)
                            .and_then(|buffer| {
                                characteristic.NotifyValueForSubscribedClientAsync(&buffer, &client)
                            })
                            .and_then(|op| op.get())
                        {
                            Ok(result) => {
                                let status = result.Status();
                                if status
                                    .as_ref()
                                    .map(|s| *s != GattCommunicationStatus::Success)
                                    .unwrap_or(true)
                                {
                                    let _ = event_tx.send(PeripheralEvent::Info(format!(
                                        "notify to {session}: {status:?}"
                                    )));
                                }
                            }
                            Err(e) => {
                                let _ = event_tx.send(PeripheralEvent::Info(format!(
                                    "notify to {session} failed: {e}"
                                )));
                            }
                        }
                    }
                }
                PeripheralCommand::SendAll(packet) => {
                    let frame = match encode_frame(&packet, DEFAULT_ATT_MTU) {
                        Ok(frame) => frame,
                        Err(e) => {
                            let _ =
                                event_tx.send(PeripheralEvent::Info(format!("encode failed: {e}")));
                            continue;
                        }
                    };
                    if let Err(e) = make_buffer(&frame)
                        .and_then(|buffer| characteristic.NotifyValueAsync(&buffer))
                        .and_then(|op| op.get())
                    {
                        let _ =
                            event_tx.send(PeripheralEvent::Info(format!("notify all failed: {e}")));
                    }
                }
                PeripheralCommand::Shutdown => break,
            }
        }

        provider.StopAdvertising().ok();
        Ok(())
    }

    fn handle_write(
        args: windows::core::Ref<'_, GattWriteRequestedEventArgs>,
        event_tx: &std::sync::mpsc::Sender<PeripheralEvent>,
    ) -> windows::core::Result<()> {
        let args = args.ok()?;
        let session = args.Session().ok().and_then(|s| session_key(&s));
        let request = args.GetRequestAsync()?.get()?;
        if let Ok(buffer) = request.Value() {
            let len = buffer.Length()? as usize;
            let mut data = vec![0u8; len];
            let reader = DataReader::FromBuffer(&buffer)?;
            reader.ReadBytes(&mut data)?;
            reader.Close()?;
            if let Some(packet) = decode_frame(&data) {
                let peer_id = hex(&packet.sender_id);
                if let Some(session) = session {
                    let _ = event_tx.send(PeripheralEvent::Packet {
                        session,
                        peer_id,
                        packet,
                    });
                }
            }
        }
        if request.Option()? == GattWriteOption::WriteWithResponse {
            request.Respond()?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
pub mod imp {
    //! Stub for non-Windows builds.
}
