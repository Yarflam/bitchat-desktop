//! Raw BLE diagnostic: list every advertisement this machine can see for 15 s.
//! Not part of the library; used to validate the radio path before mesh
//! testing (synthetic local diagnostic only).

use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _};
use futures_util::StreamExt;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = btleplug::platform::Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters.into_iter().next().expect("no BLE adapter");
    println!("adapter: {:?}", adapter.adapter_info().await);
    adapter.start_scan(Default::default()).await?;

    let mut events = adapter.events().await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut seen = std::collections::HashSet::new();

    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_secs(1), events.next()).await
        {
            if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) = event {
                if seen.insert(id.clone()) {
                    if let Ok(p) = adapter.peripheral(&id).await {
                        if let Ok(Some(props)) = p.properties().await {
                            println!(
                                "  {} rssi={:?} name={:?} services={:?} svc_data_keys={:?}",
                                props.address,
                                props.rssi,
                                props.local_name,
                                props.services,
                                props.service_data.keys().collect::<Vec<_>>()
                            );
                        }
                    }
                }
            }
        }
    }
    println!("done, {} unique devices seen", seen.len());
    Ok(())
}
