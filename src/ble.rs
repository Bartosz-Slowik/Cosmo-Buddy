//! Bluetooth LE: advertises as "Buddy". The phone writes commands to one characteristic and gets
//! notified of the battery level and of the screen contents on two others.
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use esp32_nimble::utilities::mutex::Mutex;
use esp32_nimble::utilities::BleUuid;
use esp32_nimble::{uuid128, BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties};

/// Must match the UUIDs in index.html
const SERVICE: BleUuid = uuid128!("7b6b0001-3c1a-4c2e-9a8e-6d2f5b1c0a01");
const COMMAND: BleUuid = uuid128!("7b6b0002-3c1a-4c2e-9a8e-6d2f5b1c0a01");
const STATUS: BleUuid = uuid128!("7b6b0003-3c1a-4c2e-9a8e-6d2f5b1c0a01");
const SCREEN: BleUuid = uuid128!("7b6b0004-3c1a-4c2e-9a8e-6d2f5b1c0a01");

/// Screen rows per notification: 128 bytes + 1 index byte fits even iPhones' small packets
const ROWS_PER_CHUNK: usize = 8;

pub enum Event {
    Connected,
    Disconnected,
    Command(Vec<u8>),
}

pub struct Ble {
    pub events: Receiver<Event>,
    status: Arc<Mutex<BLECharacteristic>>,
    screen: Arc<Mutex<BLECharacteristic>>,
}

impl Ble {
    pub fn start() -> Self {
        let (tx, events) = channel();
        let device = BLEDevice::take();
        let server = device.get_server();

        let connected = tx.clone();
        server.on_connect(move |server, desc| {
            // 15-30 ms intervals keep the visualizer and the screen mirror smooth
            if let Err(e) = server.update_conn_params(desc.conn_handle(), 12, 24, 0, 400) {
                log::warn!("Couldn't speed up the connection: {e:?}");
            }
            let _ = connected.send(Event::Connected);
        });
        // Advertising restarts by itself after a disconnect
        let disconnected = tx.clone();
        server.on_disconnect(move |_, _| {
            let _ = disconnected.send(Event::Disconnected);
        });

        let service = server.create_service(SERVICE);
        let command = service
            .lock()
            .create_characteristic(COMMAND, NimbleProperties::WRITE | NimbleProperties::WRITE_NO_RSP);
        command.lock().on_write(move |args| {
            let _ = tx.send(Event::Command(args.recv_data().to_vec()));
        });
        let status = service.lock().create_characteristic(STATUS, NimbleProperties::READ | NimbleProperties::NOTIFY);
        status.lock().set_value(&[255, 0, 0]);
        let screen = service.lock().create_characteristic(SCREEN, NimbleProperties::NOTIFY);

        let advertising = device.get_advertising();
        advertising.lock().set_data(BLEAdvertisementData::new().name("Buddy").add_service_uuid(SERVICE)).unwrap();
        advertising.lock().start().unwrap();
        Self { events, status, screen }
    }

    /// Battery percentage (255 when unknown) and millivolts, little-endian
    pub fn set_battery(&self, percent: Option<u8>, mv: u16) {
        let [lo, hi] = mv.to_le_bytes();
        self.status.lock().set_value(&[percent.unwrap_or(255), lo, hi]).notify();
    }

    /// Sends the framebuffer to a phone that's watching, `ROWS_PER_CHUNK` rows at a time
    pub fn mirror(&self, pixels: &[u8]) {
        let mut screen = self.screen.lock();
        if screen.subscribed_count() == 0 {
            return;
        }
        let mut packet = [0u8; 1 + ROWS_PER_CHUNK * 16];
        for (i, rows) in pixels.chunks(ROWS_PER_CHUNK * 16).enumerate() {
            packet[0] = i as u8;
            packet[1..].copy_from_slice(rows);
            screen.set_value(&packet).notify();
        }
    }
}
