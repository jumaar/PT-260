use serde_json::Value;
use tauri::{
    plugin::{Builder, Plugin},
    Manager, Runtime,
};

pub struct PrinterPlugin {
    #[cfg(target_os = "android")]
    handle: tauri::plugin::PluginHandle<tauri::Wry>,
}

// ─── Android: registra el plugin y delega al Kotlin PrinterPlugin.kt ─
#[cfg(target_os = "android")]
impl PrinterPlugin {
    pub fn bluetooth_scan(&self) -> Result<Value, String> {
        self.handle
            .run_mobile_plugin("bluetoothScan", ())
            .map_err(|e| format!("bluetoothScan: {}", e))
    }

    pub fn bluetooth_connect(&self, mac: &str) -> Result<Value, String> {
        self.handle
            .run_mobile_plugin("bluetoothConnect", serde_json::json!({ "mac": mac }))
            .map_err(|e| format!("bluetoothConnect: {}", e))
    }

    pub fn bluetooth_disconnect(&self) -> Result<Value, String> {
        self.handle
            .run_mobile_plugin("bluetoothDisconnect", ())
            .map_err(|e| format!("bluetoothDisconnect: {}", e))
    }

    pub fn bluetooth_print(&self, data: &[u8]) -> Result<Value, String> {
        let json_array: Vec<Value> = data.iter().map(|&b| Value::from(b as u64)).collect();
        self.handle
            .run_mobile_plugin("bluetoothPrint", serde_json::json!({ "data": json_array }))
            .map_err(|e| format!("bluetoothPrint: {}", e))
    }

    pub fn bluetooth_status(&self) -> Result<Value, String> {
        self.handle
            .run_mobile_plugin("bluetoothStatus", ())
            .map_err(|e| format!("bluetoothStatus: {}", e))
    }
}

// ─── Desktop: no-ops (desktop usa bluetoothctl + raw sockets) ──────
#[cfg(not(target_os = "android"))]
impl PrinterPlugin {
    pub fn bluetooth_scan(&self) -> Result<Value, String> {
        Err("Bluetooth via plugin solo disponible en Android".into())
    }
    pub fn bluetooth_connect(&self, _mac: &str) -> Result<Value, String> {
        Err("Bluetooth via plugin solo disponible en Android".into())
    }
    pub fn bluetooth_disconnect(&self) -> Result<Value, String> {
        Err("Bluetooth via plugin solo disponible en Android".into())
    }
    pub fn bluetooth_print(&self, _data: &[u8]) -> Result<Value, String> {
        Err("Bluetooth via plugin solo disponible en Android".into())
    }
    pub fn bluetooth_status(&self) -> Result<Value, String> {
        Err("Bluetooth via plugin solo disponible en Android".into())
    }
}

// ─── Plugin trait ──────────────────────────────────────────────────

impl<R: Runtime> Plugin<R> for PrinterPlugin {
    fn name(&self) -> &'static str {
        "printer"
    }
}

pub fn init() -> tauri::plugin::TauriPlugin<tauri::Wry, Value> {
    Builder::new("printer")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let plugin = {
                let handle = api.register_android_plugin(
                    "com.sistema_impresion.app",
                    "PrinterPlugin",
                )?;
                PrinterPlugin { handle }
            };

            #[cfg(not(target_os = "android"))]
            let plugin = PrinterPlugin {};

            app.manage(plugin);
            Ok(())
        })
        .build()
}
