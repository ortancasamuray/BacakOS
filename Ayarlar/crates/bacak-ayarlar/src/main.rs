mod bluetooth;

use bluetooth::{BtCmd, BtEvent, DeviceInfo};
use slint::Model;
use std::rc::Rc;
use tokio::sync::mpsc;

slint::include_modules!();

fn device_to_slint(d: &DeviceInfo) -> BtDevice {
    BtDevice {
        address: d.address.clone().into(),
        name: d.name.clone().into(),
        paired: d.paired,
        connected: d.connected,
        rssi: d.rssi as i32,
        icon: d.icon.clone().into(),
    }
}

fn main() -> anyhow::Result<()> {
    let ui = AppWindow::new()?;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BtCmd>();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<BtEvent>();

    // Tokio runtime başlat
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.spawn(async move {
        bluetooth::run(cmd_rx, event_tx).await;
    });

    // Cihaz listeleri (Slint event loop'unda çalışır)
    let paired_model = Rc::new(slint::VecModel::<BtDevice>::default());
    let nearby_model = Rc::new(slint::VecModel::<BtDevice>::default());
    ui.set_paired_devices(slint::ModelRc::from(paired_model));
    ui.set_nearby_devices(slint::ModelRc::from(nearby_model));

    // BT event'lerini Slint event loop'una aktar
    let ui_weak = ui.as_weak();
    rt.spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let ui_weak = ui_weak.clone();
            slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                handle_bt_event(&ui, event);
            })
            .ok();
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
        let _ = tx.send(BtCmd::SendFile {
            address: addr.to_string(),
            path: path.to_string(),
        });
    });

    let tx = cmd_tx.clone();
    ui.on_confirm_pairing(move |accept| { let _ = tx.send(BtCmd::ConfirmPairing(accept)); });

    let tx = cmd_tx.clone();
    ui.on_accept_incoming(move |accept| { let _ = tx.send(BtCmd::AcceptIncoming(accept)); });

    ui.on_cancel_send(move || { });

    ui.run()?;
    Ok(())
}

fn handle_bt_event(ui: &AppWindow, event: BtEvent) {
    match event {
        BtEvent::Powered(on) => {
            ui.set_bt_powered(on);
        }

        BtEvent::Scanning(s) => {
            ui.set_bt_scanning(s);
        }

        BtEvent::DeviceAdded(dev) => {
            let item = device_to_slint(&dev);
            let model = if dev.paired {
                ui.get_paired_devices()
            } else {
                ui.get_nearby_devices()
            };
            if let Some(vm) = model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
                // Zaten varsa güncelle, yoksa ekle
                let existing = (0..vm.row_count()).find(|&i| {
                    vm.row_data(i).map(|d| d.address == item.address).unwrap_or(false)
                });
                if let Some(idx) = existing {
                    vm.set_row_data(idx, item);
                } else {
                    vm.push(item);
                }
            }
        }

        BtEvent::DeviceRemoved(addr) => {
            for model in [ui.get_paired_devices(), ui.get_nearby_devices()] {
                if let Some(vm) = model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
                    let addr_shared: slint::SharedString = addr.clone().into();
                    let idx = (0..vm.row_count()).find(|&i| {
                        vm.row_data(i).map(|d| d.address == addr_shared).unwrap_or(false)
                    });
                    if let Some(i) = idx {
                        vm.remove(i);
                    }
                }
            }
        }

        BtEvent::DeviceChanged(dev) => {
            let item = device_to_slint(&dev);
            // Cihaz eşleşti mi değişti mi? İkisine de bak, yanlış yerdeyse taşı
            let paired_model = ui.get_paired_devices();
            let nearby_model = ui.get_nearby_devices();

            let in_paired = find_in_model(&paired_model, &item.address);
            let in_nearby = find_in_model(&nearby_model, &item.address);

            if dev.paired {
                // Yakın listesinden çıkar
                if let Some(i) = in_nearby {
                    if let Some(vm) = nearby_model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
                        vm.remove(i);
                    }
                }
                // Eşleşmiş listesine ekle/güncelle
                if let Some(vm) = paired_model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
                    if let Some(i) = in_paired {
                        vm.set_row_data(i, item);
                    } else {
                        vm.push(item);
                    }
                }
            } else {
                // Yakın listesinde güncelle
                if let Some(vm) = nearby_model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
                    if let Some(i) = in_nearby {
                        vm.set_row_data(i, item);
                    } else {
                        vm.push(item);
                    }
                }
            }
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
            // 3 saniye sonra gizle
            let ui_weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_toast_visible(false);
                }
            });
        }
    }
}

fn find_in_model(model: &slint::ModelRc<BtDevice>, address: &slint::SharedString) -> Option<usize> {
    if let Some(vm) = model.as_any().downcast_ref::<slint::VecModel<BtDevice>>() {
        return (0..vm.row_count()).find(|&i| {
            vm.row_data(i).map(|d| &d.address == address).unwrap_or(false)
        });
    }
    None
}
