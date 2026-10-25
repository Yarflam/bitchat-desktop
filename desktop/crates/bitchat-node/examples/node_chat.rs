//! Chat node: join the mesh, announce, handshake, and exchange private
//! messages with nearby BitChat peers.
//!
//! Usage:
//!   cargo run -p bitchat-node --example node_chat
//!   cargo run -p bitchat-node --example node_chat -- "Hello from bitchatd!"
//!       (sends the message to the first peer whose session establishes)

use std::path::PathBuf;
use std::time::Duration;

use bitchat_node::link::LinkEvent;
use bitchat_node::{MeshLink, NodeIdentity};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let message = args.get(1).cloned();
    let identity_path = PathBuf::from(
        std::env::var("BITCHATD_IDENTITY").unwrap_or_else(|_| "bitchatd.identity".to_string()),
    );
    let nickname = std::env::var("BITCHATD_NICKNAME").unwrap_or_else(|_| "bitchatd".to_string());

    let identity = NodeIdentity::load_or_generate(&identity_path)?;
    println!(
        "node identity: peer_id={} nickname={nickname}",
        identity.peer_id
    );
    println!("(identity persisted to {})", identity_path.display());

    let mut link = MeshLink::start(identity, &nickname, 256).await?;
    println!("scanning for BitChat peers...");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut sent_to: Option<String> = None;

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let timeout = remaining.min(Duration::from_secs(5));
        match tokio::time::timeout(timeout, link.events.recv()).await {
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
                    // Queueing now also triggers the Noise handshake; the
                    // message is delivered as soon as the session is up.
                    if let (Some(text), None) = (&message, &sent_to) {
                        println!("[chat    ] sending private message to {peer_id}...");
                        link.send_private_message(&peer_id, text).await?;
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
                LinkEvent::PeerDisconnected { peer_id } => {
                    println!("[link    ] {peer_id} disconnected")
                }
                LinkEvent::Info(message) => println!("[info    ] {message}"),
            },
            Ok(None) => break,
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
