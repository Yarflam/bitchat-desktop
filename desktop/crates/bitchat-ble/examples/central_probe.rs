//! Scan for BitChat peers, optionally connect, and dump mesh traffic.
//!
//! Usage:
//!   cargo run -p bitchat-ble --example central_probe                  # scan only
//!   cargo run -p bitchat-ble --example central_probe auto-connect     # scan + connect to first peer
//!   cargo run -p bitchat-ble --example central_probe connect AC:73:52:23:AC:3E
//!       # connect directly to a MAC (bonded peer), decode ANNOUNCEs

use bitchat_ble::central::{run_central, CentralCommand, CentralEventOut};
use bitchat_wire::announce::IdentityAnnouncement;
use bitchat_wire::noise::derive_peer_id;
use bitchat_wire::MessageType;

fn parse_mac(value: &str) -> Option<[u8; 6]> {
    let parts: Vec<u8> = value
        .split(':')
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    parts.try_into().ok()
}

fn describe_packet(packet: &bitchat_wire::packet::Packet) -> String {
    match MessageType::from_value(packet.packet_type) {
        Some(MessageType::Announce) => match IdentityAnnouncement::decode(&packet.payload) {
            Some(announce) => {
                let peer_id = derive_peer_id(
                    &announce
                        .signing_public_key
                        .clone()
                        .try_into()
                        .unwrap_or([0u8; 32]),
                );
                format!(
                    "ANNOUNCE nickname={:?} signing_peer_id={peer_id} caps={:?} noise_key={} bytes",
                    announce.nickname,
                    announce.capabilities,
                    announce.noise_public_key.len()
                )
            }
            None => format!(
                "ANNOUNCE (undecodable, {} payload bytes)",
                packet.payload.len()
            ),
        },
        other => format!("{other:?} payload={} bytes", packet.payload.len()),
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mode_connect_mac = args
        .iter()
        .position(|a| a == "connect")
        .and_then(|i| args.get(i + 1))
        .and_then(|m| parse_mac(m));

    println!("starting BitChat BLE central probe...");
    let mut handle = run_central(256).await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(75);
    let mut first_peer = None;

    if let Some(mac) = mode_connect_mac {
        println!("connecting directly to {mac:02x?}...");
        handle.send(CentralCommand::ConnectByAddress(mac)).await?;
    } else {
        println!("scanning for BitChat service advertisements...");
    }

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let timeout = remaining.min(std::time::Duration::from_secs(3));
        match tokio::time::timeout(timeout, handle.events.recv()).await {
            Ok(Some(event)) => match event {
                CentralEventOut::Discovered(info) => {
                    let peer = info
                        .advertised_peer_id
                        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect::<String>())
                        .unwrap_or_else(|| "(no service data)".to_string());
                    println!(
                        "[discovered] addr={} rssi={:?} peer_id={peer}",
                        info.address, info.rssi
                    );
                    if first_peer.is_none() && info.advertised_peer_id.is_some() {
                        first_peer = Some(info.id.clone());
                        println!("[probe] connecting to {}...", info.address);
                        handle.send(CentralCommand::Connect(info.id)).await?;
                    }
                }
                CentralEventOut::Connected { info, .. } => {
                    println!("[connected] {} — waiting for mesh packets...", info.address);
                }
                CentralEventOut::Disconnected { .. } => println!("[disconnected]"),
                CentralEventOut::Packet {
                    peer_id, packet, ..
                } => {
                    println!(
                        "[packet] from={peer_id} ttl={} | {}",
                        packet.ttl,
                        describe_packet(&packet)
                    );
                }
                CentralEventOut::AdapterLost(reason) => {
                    println!("[error] {reason}");
                }
            },
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let _ = handle.send(CentralCommand::Shutdown).await;
    println!("probe done.");
    Ok(())
}
