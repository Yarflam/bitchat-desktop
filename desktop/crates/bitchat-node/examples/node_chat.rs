//! Chat node: join the mesh, announce, handshake, and exchange private
//! messages with nearby BitChat peers.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_chat
//!   cargo run -p bitchat-node --example node_chat -- "Hello from bitchatd!"
//!       (sends the message to the first peer whose session establishes)
//!   cargo run -p bitchat-node --example node_chat -- "#hello mesh"
//!       (broadcasts to the common channel once a peer is announced)

use std::path::PathBuf;
use std::time::Duration;

use bitchat_node::link::LinkEvent;
use bitchat_node::{MeshLink, NodeConfig, NodeIdentity};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let message = args.get(1).cloned();
    let identity_path = PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let nickname = std::env::var("BITCHATD_NICKNAME")
        .ok()
        .filter(|nick| !nick.trim().is_empty())
        .unwrap_or_else(|| {
            let identity_path = std::path::PathBuf::from(
                std::env::var("BITCHATD_IDENTITY")
                    .unwrap_or_else(|_| "bitchatd.identity".to_string()),
            );
            NodeConfig::load(&identity_path).nickname_or_default()
        });

    let identity = NodeIdentity::load_or_generate(&identity_path)?;
    println!(
        "node identity: peer_id={} nickname={nickname}",
        identity.peer_id
    );
    println!("(identity persisted to {})", identity_path.display());

    let mut link = MeshLink::start(identity, &nickname, 256).await?;
    println!("scanning for BitChat peers...");

    // Daemon mode: run until killed (the previous 120 s demo deadline made
    // detached nodes exit on their own).
    let mut sent_to: Option<String> = None;

    loop {
        match tokio::time::timeout(Duration::from_secs(5), link.events.recv()).await {
            Ok(Some(event)) => match event {
                LinkEvent::PeerAnnounced {
                    peer_id,
                    nickname,
                    verified,
                    capabilities,
                } => {
                    println!(
                        "[announce] {nickname} ({peer_id}) verified={verified} caps={capabilities:?}"
                    );
                    if let (Some(text), None) = (&message, &sent_to) {
                        if let Some(broadcast) = text.strip_prefix('#') {
                            println!("[chat    ] broadcasting to the mesh...");
                            link.send_broadcast_message(broadcast).await?;
                        } else {
                            // Queueing now also triggers the Noise handshake;
                            // the message is delivered once the session is up.
                            println!("[chat    ] sending private message to {peer_id}...");
                            link.send_private_message(&peer_id, text).await?;
                        }
                        sent_to = Some(peer_id.clone());
                    }
                }
                LinkEvent::SessionEstablished { peer_id } => {
                    println!("[session ] Noise XX established with {peer_id}");
                }
                LinkEvent::PrivateMessage {
                    peer_id,
                    message_id,
                    content,
                } => println!("[chat    ] private from {peer_id} [{message_id}]: {content}"),
                LinkEvent::Delivered {
                    peer_id,
                    message_id,
                } => {
                    println!("[chat    ] delivered ack from {peer_id} for {message_id}")
                }
                LinkEvent::PeerStateSent { peer_id } => {
                    println!("[peerstate] sent to {peer_id}");
                }
                LinkEvent::PeerState {
                    peer_id,
                    capabilities,
                    signing_public_key,
                } => println!(
                    "[peerstate] from {peer_id} caps={capabilities:#x} key={signing_public_key}"
                ),
                LinkEvent::BroadcastMessage { peer_id, content } => {
                    println!("[chat    ] broadcast from {peer_id}: {content}")
                }
                LinkEvent::PeerDisconnected { peer_id } => {
                    println!("[link    ] {peer_id} disconnected")
                }
                LinkEvent::PeerDiscovered {
                    address,
                    peer_id,
                    rssi,
                } => println!("[scan    ] {address} peer={peer_id:?} rssi={rssi:?}"),
                LinkEvent::Info(message) => println!("[info    ] {message}"),
            },
            Ok(None) => break, // link closed
            // Elapsed: fall through to the waiting-prompt below.
            Err(_) => {
                if message.is_some() && sent_to.is_none() {
                    println!("... still waiting for a peer (keep bitchat in the foreground on the phone)");
                }
                continue;
            }
        }
    }

    link.shutdown().await;
    println!("node stopped.");
    Ok(())
}
