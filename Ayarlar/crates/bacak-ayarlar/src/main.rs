mod bluetooth;

use bluetooth::{BtCmd, BtEvent, DeviceInfo};
use tokio::sync::mpsc;

slint::include_modules!();

fn device_to_slint(d: &DeviceInfo) -> BtDevice {
    BtDevice {
        address:   d.address.clone().into(),
        name:      d.name.clone().into(),
        paired:    d.paired,
        connected: d.connected,
        rssi:      d.rssi as i32,
        icon:      d.icon.clone().into(),
    }
}

fn main() -> anyhow::Result<()> {
    let ui = AppWindow::new()?;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BtCmd>();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<BtEvent>();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.spawn(async move {
        bluetooth::run(cmd_rx, event_tx).await;
    });

    // BT event'lerini Slint event loop'una ilet
    let ui_weak = ui.as_weak();
    rt.spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let ui_weak = ui_weak.clone();
            slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                handle_bt_event(&ui, event);
            }).ok();
        }
    });

    // ─── Callback bağlantıları ─────────────────────────────────────────────

    let tx = cmd_tx.clone();
    ui.on_toggle_bluetooth(move |on| { let _ = tx.send(BtCmd::SetPowered(on)); });

    let tx = cmd_tx.clone();
    ui.on_start_scan(move || { let _ = tx.send(BtCmd::StartScan); });

    let tx = cmd_tx.clone();
    ui.on_stop_scan(move || { let _ = tx.send(BtCmd::StopScan); });

    let tx = cmd_tx.clone();
    ui.on_pair_device(move |addr| { let _ = tx.send(BtCmd::Pair(addr.to_string())); });

    let tx = cmd_tx.clone();
    ui.on_connect_device(move |addr| { let _ = tx.send(BtCmd::Connect(addr.to_string())); });

    let tx = cmd_tx.clone();
    ui.on_disconnect_device(move |addr| { let _ = tx.send(BtCmd::Disconnect(addr.to_string())); });

    let tx = cmd_tx.clone();
    ui.on_forget_device(move |addr| { let _ = tx.send(BtCmd::Forget(addr.to_string())); });

    let tx = cmd_tx.clone();
    ui.on_send_file(move |addr, path| {
        let _ = tx.send(BtCmd::SendFile { address: addr.to_string(), path: path.to_string() });
    });

    let tx = cmd_tx.clone();
    ui.on_confirm_pairing(move |accept| { let _ = tx.send(BtCmd::ConfirmPairing(accept)); });

    let tx = cmd_tx.clone();
    ui.on_accept_incoming(move |accept| { let _ = tx.send(BtCmd::AcceptIncoming(accept)); });

    ui.on_cancel_send(move || {});

    ui.run()?;
    Ok(())
}

fn handle_bt_event(ui: &AppWindow, event: BtEvent) {
    match event {
        BtEvent::Powered(on) => ui.set_bt_powered(on),

        BtEvent::Scanning(s) => ui.set_bt_scanning(s),

        BtEvent::AllDevices(devices) => {
            // Modelleri direkt yeniden oluştur — downcast yok
            let paired: slint::VecModel<BtDevice> = slint::VecModel::default();
            let nearby: slint::VecModel<BtDevice> = slint::VecModel::default();
            for d in &devices {
                if d.paired { paired.push(device_to_slint(d)); }
                else        { nearby.push(device_to_slint(d)); }
            }
            ui.set_paired_devices(slint::ModelRc::new(paired));
            ui.set_nearby_devices(slint::ModelRc::new(nearby));
        }

        BtEvent::PairingRequest { device_name, passkey } => {
            ui.set_pairing_device(device_name.into());
            ui.set_pairing_passkey(passkey.into());
            ui.set_pairing_visible(true);
        }

        BtEvent::TransferProgress { file, progress, status } => {
            ui.set_transfer_file(file.into());
            ui.set_transfer_progress(progress);
            ui.set_transfer_status(status.into());
            ui.set_transfer_visible(true);
        }

        BtEvent::IncomingFile { device, file_name } => {
            ui.set_incoming_device(device.into());
            ui.set_incoming_file(file_name.into());
            ui.set_incoming_visible(true);
        }

        BtEvent::Toast(msg) => {
            ui.set_toast_text(msg.into());
            ui.set_toast_visible(true);
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_toast_visible(false);
                }
            });
        }
    }
}
