mod bluetooth;

use bluetooth::{BtCmd, BtEvent, DeviceInfo};
use tokio::sync::mpsc;

slint::include_modules!();

// slint::Model: as_any() ve VecModel::set_vec() için gerekli
use slint::Model;

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

    // İlk AllDevices geldiğinde model oluşturmak için:
    // Slint for-döngüsünün in-place güncellemesi çalışsın diye
    // başlangıçta boş VecModel set ediyoruz.
    ui.set_paired_devices(slint::ModelRc::new(slint::VecModel::<BtDevice>::default()));
    ui.set_nearby_devices(slint::ModelRc::new(slint::VecModel::<BtDevice>::default()));

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

// Model in-place güncelle — referansı değiştirme, içeriği değiştir
fn update_model(model_rc: slint::ModelRc<BtDevice>, new_items: Vec<BtDevice>) {
    if let Some(vm) = model_rc.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
        vm.set_vec(new_items);
    } else {
        // İlk çağrıda (veya beklenmedik tipte) buraya düşmez çünkü
        // main() zaten VecModel set ediyor. Yedek olarak bırakıyoruz.
        eprintln!("[HATA] downcast VecModel başarısız — bu oluşmamalı");
    }
}

fn handle_bt_event(ui: &AppWindow, event: BtEvent) {
    match event {
        BtEvent::Powered(on) => ui.set_bt_powered(on),

        BtEvent::Scanning(s) => ui.set_bt_scanning(s),

        BtEvent::AllDevices(devices) => {
            let paired_vec: Vec<BtDevice> = devices.iter()
                .filter(|d| d.paired)
                .map(device_to_slint)
                .collect();
            let nearby_vec: Vec<BtDevice> = devices.iter()
                .filter(|d| !d.paired)
                .map(device_to_slint)
                .collect();

            eprintln!("[BT] AllDevices → eşleşmiş:{} yakın:{}", paired_vec.len(), nearby_vec.len());

            // Referansı değiştirmeden içeriği güncelle (Slint for-loop için kritik)
            update_model(ui.get_paired_devices(), paired_vec);
            update_model(ui.get_nearby_devices(), nearby_vec);
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
            eprintln!("[BT] Toast: {}", msg);
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
