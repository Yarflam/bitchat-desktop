//! Desktop GUI node: wires the mesh link into the egui app.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_gui

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::thread;

use bitchat_node::gui::{egui, parse_mac, terminal_style, BitchatApp, UiCommand};
use bitchat_node::{LinkEvent, MeshLink, NodeIdentity};

fn main() -> eframe::Result<()> {
    let identity_path = PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let nickname = BitchatApp::load_nickname(&identity_path);
    let identity = NodeIdentity::load_or_generate(&identity_path)
        .expect("load or generate node identity");
    let our_id = identity.peer_id.clone();
    eprintln!("bitchatd: peer_id={our_id} nickname={nickname}");

    #[cfg(windows)]
    {
        use windows::ApplicationModel::Package;
        match Package::Current() {
            Ok(package) => {
                let id = package.Id().expect("package id");
                eprintln!(
                    "bitchatd: identity {} ({})",
                    id.Name().expect("name"),
                    id.Publisher().expect("publisher")
                );
            }
            Err(error) => eprintln!("bitchatd: NO PACKAGE IDENTITY: {error}"),
        }
    }

    // Node engine on its own thread, started lazily from the first UI frame
    // (after winit finished initializing): starting the WinRT peripheral while
    // winit is still initializing gets the publisher denied (E_ACCESSDENIED).
    let (event_tx, event_rx) = std_mpsc::channel::<LinkEvent>();
    let (cmd_tx, cmd_rx) = std_mpsc::channel::<UiCommand>();
    let node_nickname = nickname.clone();
    let (start_tx, start_rx) = std_mpsc::channel::<()>();
    thread::spawn(move || {
        let _ = start_rx.recv();
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        runtime.block_on(async move {
            let mut link = match MeshLink::start(identity, &node_nickname, 256).await {
                Ok(link) => link,
                Err(error) => {
                    eprintln!("bitchatd: failed to start mesh link: {error}");
                    return;
                }
            };
            let (async_cmd_tx, mut async_cmd_rx) = tokio::sync::mpsc::channel::<UiCommand>(64);
            thread::spawn(move || {
                for command in cmd_rx {
                    if async_cmd_tx.blocking_send(command).is_err() {
                        break;
                    }
                }
            });
            loop {
                tokio::select! {
                    event = link.events.recv() => match event {
                        Some(event) => {
                            // Mirror link events into the log file for debugging.
                            match &event {
                                LinkEvent::Info(message) => eprintln!("[info] {message}"),
                                LinkEvent::PeerAnnounced { peer_id, nickname, verified, .. } => {
                                    eprintln!("[announce] {nickname} ({peer_id}) verified={verified}")
                                }
                                LinkEvent::SessionEstablished { peer_id } => {
                                    eprintln!("[session] established with {peer_id}")
                                }
                                LinkEvent::PrivateMessage { peer_id, content, .. } => {
                                    eprintln!("[chat] private from {peer_id}: {content}")
                                }
                                LinkEvent::BroadcastMessage { peer_id, content } => {
                                    eprintln!("[chat] broadcast from {peer_id}: {content}")
                                }
                                LinkEvent::PrivateFile { peer_id, file } => {
                                    eprintln!(
                                        "[file] private from {peer_id}: {} ({} bytes, {})",
                                        file.file_name, file.file_size, file.mime_type
                                    )
                                }
                                LinkEvent::BroadcastFile { peer_id, file } => {
                                    eprintln!(
                                        "[file] broadcast from {peer_id}: {} ({} bytes, {})",
                                        file.file_name, file.file_size, file.mime_type
                                    )
                                }
                                LinkEvent::Delivered { peer_id, .. } => {
                                    eprintln!("[chat] delivered -> {peer_id}")
                                }
                                LinkEvent::PeerStateSent { peer_id } => {
                                    eprintln!("[peerstate] sent to {peer_id}")
                                }
                                LinkEvent::PeerState {
                                    peer_id,
                                    capabilities,
                                    signing_public_key,
                                } => {
                                    eprintln!(
                                        "[peerstate] from {peer_id} caps={capabilities:#x} key={signing_public_key}"
                                    )
                                }
                                LinkEvent::PeerDisconnected { peer_id } => {
                                    eprintln!("[link] {peer_id} disconnected")
                                }
                                LinkEvent::PeerDiscovered { address, peer_id, rssi } => {
                                    eprintln!("[scan] {address} peer={peer_id:?} rssi={rssi:?}")
                                }
                            }
                            if event_tx.send(event).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                    command = async_cmd_rx.recv() => match command {
                        Some(UiCommand::SendPrivate { peer_id, content }) => {
                            if let Err(error) = link.send_private_message(&peer_id, &content).await
                            {
                                eprintln!("bitchatd: send private to {peer_id} failed: {error}");
                            }
                        }
                        Some(UiCommand::SendBroadcast { content }) => {
                            if let Err(error) = link.send_broadcast_message(&content).await {
                                eprintln!("bitchatd: send broadcast failed: {error}");
                            }
                        }
                        Some(UiCommand::SendPrivateFile { peer_id, file }) => {
                            if let Err(error) = link.send_private_file(&peer_id, file).await {
                                eprintln!("bitchatd: send private file to {peer_id} failed: {error}");
                            }
                        }
                        Some(UiCommand::SendBroadcastFile { file }) => {
                            if let Err(error) = link.send_broadcast_file(file).await {
                                eprintln!("bitchatd: send broadcast file failed: {error}");
                            }
                        }
                        Some(UiCommand::SetNickname { nickname }) => {
                            if let Err(error) = link.set_nickname(&nickname).await {
                                eprintln!("bitchatd: set nickname failed: {error}");
                            }
                        }
                        Some(UiCommand::Connect { address }) => {
                            let parsed = parse_mac(&address);
                            match parsed {
                                Some(mac) => {
                                    if let Err(error) = link.connect_by_address(mac).await {
                                        eprintln!("bitchatd: connect to {address} failed: {error}");
                                    }
                                }
                                None => eprintln!("bitchatd: invalid MAC address '{address}'"),
                            }
                        }
                        None => break,
                    },
                }
            }
        });
    });

    let app = BitchatApp::new(
        event_rx,
        cmd_tx,
        Some(start_tx),
        our_id,
        nickname,
        &identity_path,
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1160.0, 760.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "bitchatd",
        options,
        Box::new(|cc| {
            terminal_style(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
