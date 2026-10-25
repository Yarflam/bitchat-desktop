//! Minimal WinRT GATT server: advertise the BitChat service and dump incoming
//! writes. Validation tool: point nRF Connect (or the phone's BitChat) at the
//! PC and confirm the service shows up and accepts writes.
//!
//! Windows only.

#[cfg(not(windows))]
fn main() {
    eprintln!("this example requires Windows (WinRT GATT server)");
}

#[cfg(windows)]
#[tokio::main(flavor = "multi_thread")]
async fn main() -> windows::core::Result<()> {
    use windows::core::GUID;
    use windows::Devices::Bluetooth::GenericAttributeProfile::*;
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::DataReader;

    const SERVICE: GUID = GUID::from_u128(0xF47B5E2D_4A9E_4C5A_9B3F_8E1D2C3A4B5C);
    const CHARACTERISTIC: GUID = GUID::from_u128(0xA1B2C3D4_E5F6_4A5B_8C9D_0E1F2A3B4C5D);

    println!("creating GATT service provider...");
    let create_result =
        tokio::task::spawn_blocking(move || GattServiceProvider::CreateAsync(SERVICE)?.get())
            .await
            .expect("join")?;
    let provider = create_result.ServiceProvider()?;

    let service = provider.Service()?;
    let params = GattLocalCharacteristicParameters::new()?;
    params.SetCharacteristicProperties(
        GattCharacteristicProperties::Write
            | GattCharacteristicProperties::WriteWithoutResponse
            | GattCharacteristicProperties::Notify,
    )?;
    params.SetWriteProtectionLevel(GattProtectionLevel::Plain)?;
    let char_result = tokio::task::spawn_blocking({
        let service = service.clone();
        move || {
            service
                .CreateCharacteristicAsync(CHARACTERISTIC, &params)
                .and_then(|op| op.get())
        }
    })
    .await
    .expect("join")?;
    let characteristic = char_result.Characteristic()?;
    println!("characteristic created");

    characteristic.WriteRequested(&TypedEventHandler::new(
        |_: windows::core::Ref<'_, GattLocalCharacteristic>,
         args: windows::core::Ref<'_, GattWriteRequestedEventArgs>| {
            let args = args.ok()?;
            let request = args.GetRequestAsync()?.get()?;
            if let Ok(buffer) = request.Value() {
                let len = buffer.Length()? as usize;
                let mut data = vec![0u8; len];
                let reader = DataReader::FromBuffer(&buffer)?;
                reader.ReadBytes(&mut data)?;
                println!("[write] {len} bytes: {}", hex(&data));
                reader.Close()?;
            }
            if request.Option()? == GattWriteOption::WriteWithResponse {
                request.Respond()?;
            }
            Ok(())
        },
    ))?;

    provider.AdvertisementStatusChanged(&TypedEventHandler::new(
        |sender: windows::core::Ref<'_, GattServiceProvider>, _| {
            let sender = sender.ok()?;
            println!("[adv] status = {:?}", sender.AdvertisementStatus()?);
            Ok(())
        },
    ))?;

    let subscribed = characteristic.clone();
    characteristic.SubscribedClientsChanged(&TypedEventHandler::new(
        move |_: windows::core::Ref<'_, GattLocalCharacteristic>, _| {
            let count = subscribed.SubscribedClients()?.Size()?;
            println!("[subscribed] clients = {count}");
            Ok(())
        },
    ))?;

    let adv_params = GattServiceProviderAdvertisingParameters::new()?;
    adv_params.SetIsDiscoverable(true)?;
    adv_params.SetIsConnectable(true)?;
    provider.StartAdvertisingWithParameters(&adv_params)?;
    println!("advertising; waiting for connections (Ctrl+C to stop)...");

    tokio::signal::ctrl_c().await.ok();
    provider.StopAdvertising()?;
    println!("stopped.");
    Ok(())
}

#[cfg(windows)]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
