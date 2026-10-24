//! Scan for BitChat peers, connect to the first one, and dump mesh traffic.
//!
//! Usage: cargo run -p bitchat-ble --example central_probe [auto-connect]
//! Without arguments it only scans. With `auto-connect` it also connects to
//! the first discovered BitChat peer and prints received packets for 60 s.

use bitchat_ble::central::{run_central, CentralCommand, CentralEventOut};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auto_connect = std::env::args().any(|a| a == "auto-connect");

    println!(
        "starting BitChat BLE central probe (scan only = {})...",
        !auto_connect
    );
    let mut handle = run_central(256).await?;
    println!("scanning for BitChat service advertisements...");

    let mut first_peer = None;
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(if auto_connect { 60 } else { 20 });

    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let timeout = remaining.min(std::time::Duration::from_secs(2));
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
                    if auto_connect && first_peer.is_none() && info.advertised_peer_id.is_some() {
                        first_peer = Some(info.id.clone());
                        println!("[probe] connecting to {}...", info.address);
                        handle.send(CentralCommand::Connect(info.id)).await?;
                    }
                }
                CentralEventOut::Connected { info, .. } => {
                    println!("[connected] {}", info.address);
                }
                CentralEventOut::Disconnected { .. } => {
                    println!("[disconnected]");
                }
                CentralEventOut::Packet {
                    peer_id, packet, ..
                } => {
                    println!(
                        "[packet] from={peer_id} type=0x{:02x} ttl={} payload={} bytes",
                        packet.packet_type,
                        packet.ttl,
                        packet.payload.len()
                    );
                }
                CentralEventOut::AdapterLost(reason) => {
                    println!("[adapter lost] {reason}");
                    break;
                }
            },
            Ok(None) => break,
            Err(_) => continue, // scan window tick
        }
    }

    let _ = handle.send(CentralCommand::Shutdown).await;
    println!("probe done.");
    Ok(())
}
