use base64::Engine;
use image::ImageReader;
use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use tauri::Emitter;

mod plugin_printer;
use crate::plugin_printer::PrinterPlugin;

// MAC del último dispositivo Bluetooth conectado (para reconexión automática)
static LAST_BT_MAC: Mutex<Option<String>> = Mutex::new(None);

// Socket RFCOMM persistente: se crea una sola vez y se reutiliza en todas
// las impresiones. Evita el ciclo disconnect/reconnect que causaba EBUSY.
static BT_SOCKET_CACHE: Mutex<Option<(i32, String)>> = Mutex::new(None);

// ─── LOG MACROS ────────────────────────────────────────────────────
// Usamos eprintln! para que los logs aparezcan en stderr (visible
// en la terminal incluso si stdout está capturado por Tauri).

macro_rules! log_info {
    ($($arg:tt)*) => {
        eprintln!("[INFO  ][rust] {}", format!($($arg)*))
    };
}

macro_rules! log_warn {
    ($($arg:tt)*) => {
        eprintln!("[WARN  ][rust] {}", format!($($arg)*))
    };
}

macro_rules! log_error {
    ($($arg:tt)*) => {
        eprintln!("[ERROR ][rust] {}", format!($($arg)*))
    };
}

macro_rules! log_cmd {
    ($cmd:expr) => {
        eprintln!("[CMD   ][rust] invoke recibido: `{}`", $cmd)
    };
}

// ─── PERSISTENCIA DE MAC BLUETOOTH ──────────────────────────────────
// Guarda las MACs de dispositivos que se conectaron exitosamente en
// ~/.config/rust-hello/bt-devices.json para reconexión automática al iniciar.

fn bt_persist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config/rust-hello/bt-devices.json")
}

fn load_persisted_macs() -> Vec<String> {
    let path = bt_persist_path();
    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(json) => {
                if let Some(macs) = json.get("macs").and_then(|v| v.as_array()) {
                    let list: Vec<String> = macs
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    log_info!("persistencia: {} MAC(s) cargada(s) del disco", list.len());
                    return list;
                }
            }
            Err(e) => log_warn!("persistencia: error parseando JSON: {}", e),
        },
        Err(_) => log_info!("persistencia: no hay archivo previo (primer arranque)"),
    }
    vec![]
}

fn save_persisted_mac(mac: &str) {
    let path = bt_persist_path();
    let mut macs = load_persisted_macs();

    if !macs.contains(&mac.to_string()) {
        macs.push(mac.to_string());
    }
    if macs.len() > 5 {
        macs = macs[macs.len() - 5..].to_vec();
    }

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let json = serde_json::json!({"macs": macs});
    if let Ok(data) = serde_json::to_string_pretty(&json) {
        if let Err(e) = fs::write(&path, data) {
            log_warn!("persistencia: no se pudo escribir archivo: {}", e);
        } else {
            log_info!("persistencia: MAC {} guardada en disco", mac);
        }
    }
}

/// Intenta reconectar a una MAC Bluetooth guardada previamente.
/// Primero verifica si ya está conectada. Si no, empareja y conecta.
/// Retorna true si logró conectar, false en caso contrario.
fn bt_try_reconnect(mac: &str) -> bool {
    log_info!("bluetooth: intentando reconectar a {}...", mac);

    if bt_is_connected(mac) {
        log_info!("bluetooth: {} ya esta conectado", mac);
        return true;
    }

    match bluetooth_pair(mac) {
        Ok(()) => {
            let _ = Command::new("bluetoothctl")
                .args(["connect", mac])
                .output();
            std::thread::sleep(std::time::Duration::from_millis(500));

            if bt_is_connected(mac) {
                log_info!("bluetooth: reconectado exitosamente a {}", mac);
                return true;
            }

            bt_reset_link(mac);
            bt_is_connected(mac)
        }
        Err(e) => {
            log_warn!("bluetooth: no se pudo emparejar con {}: {}", mac, e);
            false
        }
    }
}

// ─── DETECCIÓN DE HARDWARE ─────────────────────────────────────────
// IDs de la impresora GEZHI (mismos que en el sistema Python)
const IMPRESORA_VID: &str = "0483";
const IMPRESORA_PID: &str = "5720";
// Offset horizontal para el comando BITMAP (idéntico al X_OFFSET de Python)
const BITMAP_X_OFFSET: i32 = -20;
// Offset vertical (margen superior) en dots (8 dots = 1 mm)
const BITMAP_Y_OFFSET: i32 = 5;

/// Verifica si el dispositivo USB con el VID/PID dado está físicamente conectado.
/// Usa `lsusb` (igual que el sistema Python) — el método más fiable en Linux.
fn usb_device_present(vid: &str, pid: &str) -> bool {
    let target = format!("{}:{}", vid, pid).to_lowercase();

    match Command::new("lsusb").output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let found = stdout.to_lowercase().contains(&target);

            if found {
                // Extraer la línea exacta de lsusb para mostrarla en logs
                for line in stdout.lines() {
                    if line.to_lowercase().contains(&target) {
                        log_info!("lsusb: {}", line.trim());
                        break;
                    }
                }
                log_info!("dispositivo VID:{} PID:{} encontrado via lsusb", vid, pid);
            } else {
                log_info!(
                    "lsusb: dispositivo VID:{} PID:{} NO encontrado. \
                     ¿Esta la impresora conectada y encendida?",
                    vid,
                    pid
                );
            }
            found
        }
        Err(e) => {
            log_error!("no se pudo ejecutar 'lsusb': {}. Verifique que usbutils este instalado.", e);
            false
        }
    }
}

/// Verifica si existe al menos un dispositivo /dev/usb/lpX (driver usblp cargado).
fn usblp_device_exists() -> Option<String> {
    for i in 0..10 {
        let path = format!("/dev/usb/lp{}", i);
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }
    None
}

// ─── BLUETOOTH ──────────────────────────────────────────────────────
// Comunicación Bluetooth SPP (Serial Port Profile) para impresoras térmicas.
// Usa bluetoothctl + rfcomm en Linux. En Android se usa PrinterPlugin.kt.
//
// Flujo:
//   1. bluetoothctl scan on    → descubrir dispositivos
//   2. bluetoothctl pair <MAC> → emparejar
//   3. sudo rfcomm bind 0 <MAC> 1 → crear /dev/rfcomm0 (SPP)
//   4. Escribir a /dev/rfcomm0  → mismos comandos TSPL que USB

/// Escanea dispositivos Bluetooth cercanos durante `timeout_secs` segundos.
/// Retorna una lista de (dirección MAC, nombre).
fn bluetooth_scan(timeout_secs: u64) -> Vec<(String, String)> {
    log_info!("bluetooth: iniciando escaneo ({}s)...", timeout_secs);

    let output = match Command::new("bluetoothctl")
        .args(["--timeout", &timeout_secs.to_string(), "scan", "on"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log_error!("bluetooth: no se pudo ejecutar bluetoothctl: {}", e);
            log_error!("bluetooth: ¿Está bluetoothctl instalado? sudo apt install bluez");
            return vec![];
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut devices: Vec<(String, String)> = vec![];
    let mut seen = std::collections::HashSet::new();

    for line in stdout.lines() {
        // Formato típico: "[NEW] Device 00:11:22:33:44:55 Nombre del dispositivo"
        if line.contains("Device") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Buscar patrón: ... "Device" <MAC> <nombre...>
            if let Some(pos) = parts.iter().position(|&p| p == "Device") {
                if pos + 1 < parts.len() {
                    let mac = parts[pos + 1].to_string();
                    if mac.len() == 17 && mac.contains(':') && !seen.contains(&mac) {
                        seen.insert(mac.clone());
                        let name = parts[pos + 2..].join(" ");
                        log_info!("bluetooth: encontrado {} ({})", mac, name);
                        devices.push((mac, name));
                    }
                }
            }
        }
    }

    if devices.is_empty() {
        log_warn!("bluetooth: no se encontraron dispositivos durante el escaneo.");
        log_warn!("bluetooth: Asegúrese de que la impresora esté encendida y en modo emparejable.");
    }

    devices
}

/// Empareja con un dispositivo Bluetooth por su dirección MAC.
fn bluetooth_pair(mac: &str) -> Result<(), String> {
    log_info!("bluetooth: emparejando con {} ...", mac);

    let output = Command::new("bluetoothctl")
        .args(["pair", mac])
        .output()
        .map_err(|e| format!("No se pudo ejecutar bluetoothctl pair: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    log_info!("bluetooth: pair stdout: {}", stdout.trim());
    if !stderr.trim().is_empty() {
        log_warn!("bluetooth: pair stderr: {}", stderr.trim());
    }

    if stdout.contains("Pairing successful") || stdout.contains("Paired: yes") {
        log_info!("bluetooth: emparejado exitosamente con {}", mac);
        Ok(())
    } else {
        // Puede que ya esté emparejado, intentar confiar
        log_info!("bluetooth: pair no confirmó explícitamente. Intentando trust...");
        let _ = Command::new("bluetoothctl")
            .args(["trust", mac])
            .output();
        Ok(())
    }
}

/// Protocolo RFCOMM sobre L2CAP (Bluetooth Serial Port).
const BTPROTO_RFCOMM: i32 = 3;

/// Parsea una MAC "AA:BB:CC:DD:EE:FF" y la devuelve en formato bdaddr
/// (little-endian, 6 bytes), tal como la espera el kernel Linux.
fn bt_parse_mac(mac: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    out.reverse(); // bdaddr se almacena little-endian
    Some(out)
}

/// Consulta `bluetoothctl info <mac>` y dice si el dispositivo está conectado.
fn bt_is_connected(mac: &str) -> bool {
    let out = match Command::new("bluetoothctl").args(["info", mac]).output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    String::from_utf8_lossy(&out.stdout).contains("Connected: yes")
}

/// Resetea el enlace BlueZ: disconnect + connect. Necesario cuando la sesión
/// RFCOMM del kernel queda en estado stale (provoca EBUSY al conectar un
/// socket nuevo). No requiere root.
fn bt_reset_link(mac: &str) {
    log_info!("bluetooth: reseteando enlace BlueZ para {} ...", mac);
    let _ = Command::new("bluetoothctl").args(["disconnect", mac]).output();
    std::thread::sleep(std::time::Duration::from_millis(800));
    let out = Command::new("bluetoothctl").args(["connect", mac]).output();
    if let Ok(o) = out {
        let s = String::from_utf8_lossy(&o.stdout);
        if !s.trim().is_empty() {
            log_info!("bluetooth: connect -> {}", s.trim());
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
}

/// Busca entre los dispositivos emparejados alguno que esté conectado
/// (la impresora). Devuelve su MAC para poder imprimir sin conexión manual.
fn bt_find_connected_printer() -> Option<String> {
    let out = Command::new("bluetoothctl")
        .args(["devices", "Paired"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = parts.iter().position(|p| *p == "Device") {
            if let Some(mac) = parts.get(pos + 1) {
                if mac.len() == 17 && mac.contains(':') && bt_is_connected(mac) {
                    log_info!("bluetooth: impresora conectada detectada: {}", mac);
                    return Some(mac.to_string());
                }
            }
        }
    }
    None
}

/// Cierra el socket RFCOMM cacheado (si existe) y limpia el cache.
fn bt_close_cached_socket() {
    if let Ok(mut cache) = BT_SOCKET_CACHE.lock() {
        if let Some((fd, mac)) = cache.take() {
            unsafe { libc::close(fd); }
            log_info!("bluetooth: socket persistente cerrado para {}", mac);
        }
    }
}

/// Establece el socket RFCOMM persistente de inmediato (sin esperar a la
/// primera impresión). Crea el socket, lo conecta y lo guarda en cache.
fn bt_preconnect_socket(mac: &str, channel: u8) -> Result<(), String> {
    {
        let cache = BT_SOCKET_CACHE.lock().map_err(|e| format!("lock: {}", e))?;
        if let Some((fd, cached_mac)) = cache.as_ref() {
            if cached_mac == mac && bt_socket_healthy(*fd) {
                log_info!("bluetooth: socket persistente ya existe para {}", mac);
                return Ok(());
            }
        }
    }

    bt_close_cached_socket();

    let fd = bt_create_rfcomm_socket(mac, channel)?;
    if let Ok(mut cache) = BT_SOCKET_CACHE.lock() {
        *cache = Some((fd, mac.to_string()));
    } else {
        unsafe { libc::close(fd); }
        return Err("No se pudo cachear el socket".to_string());
    }
    log_info!("bluetooth: socket persistente establecido para {}", mac);
    Ok(())
}

/// Verifica si un fd de socket sigue sano (sin errores pendientes).
fn bt_socket_healthy(fd: i32) -> bool {
    let mut err: i32 = 0;
    let mut err_len: libc::socklen_t = 4;
    unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut err as *mut _ as *mut libc::c_void,
            &mut err_len,
        ) == 0 && err == 0
    }
}

/// Crea un socket RFCOMM nuevo hacia `mac:channel` y lo conecta.
/// Si connect devuelve EBUSY (sesión stale), resetea el enlace BlueZ y
/// reintenta una vez.
fn bt_create_rfcomm_socket(mac: &str, channel: u8) -> Result<i32, String> {
    let bdaddr = bt_parse_mac(mac).ok_or_else(|| format!("MAC inválida: {}", mac))?;

    let mut last_err = String::new();
    for attempt in 0..2 {
        if attempt == 1 {
            bt_reset_link(mac);
        }

        let fd = unsafe { libc::socket(libc::AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
        if fd < 0 {
            last_err = format!("socket(): {}", std::io::Error::last_os_error());
            continue;
        }

        let mut sa = [0u8; 10];
        sa[0..2].copy_from_slice(&(libc::AF_BLUETOOTH as u16).to_ne_bytes());
        sa[2..8].copy_from_slice(&bdaddr);
        sa[8] = channel;

        let rc = unsafe { libc::connect(fd, sa.as_ptr() as *const libc::sockaddr, 10) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            last_err = format!("connect RFCOMM: {}", e);
            unsafe { libc::close(fd) };
            if e.raw_os_error() == Some(16) {
                log_warn!("bt_create_rfcomm_socket: EBUSY en connect (intento {}), reseteando enlace...", attempt + 1);
                continue;
            }
            return Err(last_err);
        }

        return Ok(fd);
    }

    Err(format!(
        "No se pudo conectar RFCOMM a {}: {}",
        mac, last_err
    ))
}

/// Envía datos crudos a la impresora por Bluetooth RFCOMM.
/// Usa un socket persistente: lo crea una sola vez y lo reutiliza en todas
/// las impresiones. Si el socket cacheado falla, lo recrea automáticamente.
fn bt_rfcomm_send(mac: &str, channel: u8, data: &[u8]) -> Result<(), String> {
    // ── Intentar usar socket cacheado ──────────────────────────
    let cached_fd: Option<i32> = {
        let cache = BT_SOCKET_CACHE.lock().map_err(|e| format!("lock: {}", e))?;
        cache.as_ref().and_then(|(fd, cached_mac)| {
            if cached_mac == mac { Some(*fd) } else { None }
        })
    };

    if let Some(fd) = cached_fd {
        if bt_socket_healthy(fd) {
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            match file.write_all(data).and_then(|_| file.flush()) {
                Ok(()) => {
                    let raw = file.into_raw_fd();
                    // Devolver al cache
                    if let Ok(mut cache) = BT_SOCKET_CACHE.lock() {
                        if cache.as_ref().map_or(true, |(_, m)| m == mac) {
                            *cache = Some((raw, mac.to_string()));
                        } else {
                            unsafe { libc::close(raw); }
                        }
                    } else {
                        unsafe { libc::close(raw); }
                    }
                    log_info!(
                        "bt_rfcomm_send: {} bytes enviados a {} canal {} (socket reutilizado)",
                        data.len(), mac, channel
                    );
                    return Ok(());
                }
                Err(e) => {
                    let _ = file.into_raw_fd();
                    log_warn!("bt_rfcomm_send: write falló en socket cacheado ({}), recreando...", e);
                }
            }
        }
        // Socket muerto o write falló: cerrar y limpiar cache
        bt_close_cached_socket();
    }

    // ── Crear socket nuevo ─────────────────────────────────────
    let fd = bt_create_rfcomm_socket(mac, channel)?;

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    match file.write_all(data).and_then(|_| file.flush()) {
        Ok(()) => {
            let raw = file.into_raw_fd();
            // Guardar en cache para reutilizar
            if let Ok(mut cache) = BT_SOCKET_CACHE.lock() {
                if cache.as_ref().map_or(true, |(_, m)| m == mac) {
                    *cache = Some((raw, mac.to_string()));
                } else {
                    unsafe { libc::close(raw); }
                }
            } else {
                unsafe { libc::close(raw); }
            }
            log_info!(
                "bt_rfcomm_send: {} bytes enviados a {} canal {} (socket nuevo)",
                data.len(), mac, channel
            );
            Ok(())
        }
        Err(e) => {
            log_error!("bt_rfcomm_send: write falló en socket nuevo: {}", e);
            Err(format!("write RFCOMM: {}", e))
        }
    }
}

// ─── COMANDOS TAURI ────────────────────────────────────────────────

#[tauri::command]
fn check_printer_status(
    state: tauri::State<'_, PrinterPlugin>,
) -> serde_json::Value {
    log_cmd!("check_printer_status");

    let usb_connected: bool;
    let hw_present: bool;
    let lp_device: Option<String>;

    // Bluetooth: detectar impresora emparejada y conectada
    let mut bt_mac: Option<String> = None;

    #[cfg(not(target_os = "android"))]
    {
        hw_present = usb_device_present(IMPRESORA_VID, IMPRESORA_PID);
        lp_device = usblp_device_exists();
        usb_connected = hw_present && lp_device.is_some();
    }
    #[cfg(target_os = "android")]
    {
        hw_present = false;
        lp_device = None;
        usb_connected = false;

        state.request_bt_permission();

        if let Ok(status) = state.bluetooth_status() {
            if status.get("connected").and_then(|c| c.as_bool()).unwrap_or(false) {
                bt_mac = Some("android-bt".to_string());
            }
        }
    }

    let bt_connected = bt_mac.is_some();
    let connected = usb_connected || bt_connected;

    #[cfg(not(target_os = "android"))]
    {
        if hw_present {
            log_info!(
                "check_printer_status → HW detectado (VID:{}, PID:{})",
                IMPRESORA_VID,
                IMPRESORA_PID
            );
            match &lp_device {
                Some(path) => {
                    log_info!("check_printer_status → driver usblp cargado: {}", path);
                }
                None => {
                    log_warn!(
                        "check_printer_status → HW presente pero driver usblp NO cargado. \
                         Ejecute: sudo modprobe usblp"
                    );
                }
            }
        } else {
            log_warn!(
                "check_printer_status → HW NO detectado. \
                 Buscando VID:{} PID:{}",
                IMPRESORA_VID,
                IMPRESORA_PID
            );
        }
    }

    if connected {
        log_info!("check_printer_status → IMPRESORA CONECTADA y lista");
    } else {
        log_warn!("check_printer_status → IMPRESORA NO LISTA");
    }

    serde_json::json!({
        "connected": connected,
        "usb_connected": usb_connected,
        "bt_connected": bt_connected,
        "hw_present": hw_present,
        "lp_device": lp_device,
        "bt_device": bt_mac,
        "connection_type": if bt_connected { "bluetooth" } else if usb_connected { "usb" } else { "none" }
    })
}

// ─── COMANDOS BLUETOOTH ─────────────────────────────────────────────

#[tauri::command]
#[allow(unused_variables)]
fn bluetooth_scan_devices(
    state: tauri::State<'_, PrinterPlugin>,
    timeout_secs: Option<u64>,
) -> serde_json::Value {
    log_cmd!("bluetooth_scan_devices");

    #[cfg(target_os = "android")]
    {
        match state.bluetooth_scan() {
            Ok(resp) => {
                let devices = resp
                    .get("devices")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();
                log_info!("bluetooth: {} dispositivos via plugin", devices.len());
                return serde_json::json!({
                    "devices": devices,
                    "_diag": {
                        "adapter_available": resp.get("adapter_available"),
                        "adapter_enabled": resp.get("adapter_enabled"),
                        "adapter_name": resp.get("adapter_name"),
                        "adapter_address": resp.get("adapter_address"),
                        "bonded_device_count": resp.get("bonded_device_count"),
                        "error": resp.get("error"),
                    }
                });
            }
            Err(e) => {
                log_error!("bluetooth scan via plugin: {}", e);
                return serde_json::json!({"devices": [], "_error": e});
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        let timeout = timeout_secs.unwrap_or(10);
        let devices: Vec<serde_json::Value> = bluetooth_scan(timeout)
            .into_iter()
            .map(|(mac, name)| {
                serde_json::json!({"mac": mac, "name": name})
            })
            .collect();
        serde_json::json!({"devices": devices})
    }
}

#[tauri::command]
fn bluetooth_connect_printer(
    state: tauri::State<'_, PrinterPlugin>,
    mac: String,
) -> Result<String, String> {
    log_cmd!("bluetooth_connect_printer");
    log_info!("bluetooth: solicitada conexion a {}", mac);

    #[cfg(target_os = "android")]
    {
        match state.bluetooth_connect(&mac) {
            Ok(resp) => {
                let connected = resp
                    .get("connected")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false);
                if connected {
                    log_info!("bluetooth: conectado via plugin a {}", mac);
                    return Ok(format!("bluetooth:{}", mac));
                }
                Err("No se pudo conectar via Bluetooth".to_string())
            }
            Err(e) => Err(format!("Error conectando Bluetooth: {}", e)),
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        // Emparejar primero
        bluetooth_pair(&mac)?;

        // Guardar MAC para impresion por socket RFCOMM directo
        if let Ok(mut stored) = LAST_BT_MAC.lock() {
            *stored = Some(mac.clone());
        }

        // Persistir la MAC en disco para reconexion automatica en proximos arranques
        save_persisted_mac(&mac);

        // Asegurar enlace BlueZ. Con socket persistente ya no se resetea
        // el enlace al estar conectado (eso causaba el ciclo disconnect/reconnect).
        if !bt_is_connected(&mac) {
            let _ = Command::new("bluetoothctl").args(["connect", &mac]).output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            if !bt_is_connected(&mac) {
                bt_reset_link(&mac);
            }
        }

        // Cerrar socket cacheado si es de otra MAC (se recreara al imprimir)
        {
            let cache = BT_SOCKET_CACHE.lock().ok();
            if let Some(cache) = cache {
                if cache.as_ref().map_or(false, |(_, m)| m != &mac) {
                    drop(cache);
                    bt_close_cached_socket();
                }
            }
        }

        log_info!("bluetooth: impresora lista (RFCOMM canal 1, socket directo sin root)");

        // Establecer socket RFCOMM persistente AHORA, no en la primera impresion
        if let Err(e) = bt_preconnect_socket(&mac, 1) {
            log_warn!("bluetooth: no se pudo pre-conectar socket RFCOMM: {}", e);
        }

        Ok(format!("bluetooth:{}", mac))
    }
}

#[tauri::command]
fn bluetooth_disconnect_printer(
    state: tauri::State<'_, PrinterPlugin>,
) -> Result<(), String> {
    log_cmd!("bluetooth_disconnect_printer");

    #[cfg(target_os = "android")]
    {
        match state.bluetooth_disconnect() {
            Ok(_) => {
                log_info!("bluetooth: desconectado via plugin");
                Ok(())
            }
            Err(e) => {
                log_warn!("bluetooth: error al desconectar via plugin: {}", e);
                Ok(())
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    {
        let mac = LAST_BT_MAC.lock().ok().and_then(|m| m.clone());

        bt_close_cached_socket();

        if let Some(mac) = mac {
            log_info!("bluetooth: desconectando {} ...", mac);
            let _ = Command::new("bluetoothctl")
                .args(["disconnect", &mac])
                .output();
        }

        if let Ok(mut stored) = LAST_BT_MAC.lock() {
            *stored = None;
        }
        log_info!("bluetooth: desconectado");
        Ok(())
    }
}


#[tauri::command]
fn imprimir_etiqueta_raw(
    state: tauri::State<'_, PrinterPlugin>,
    base64_image: String,
    width_mm: u32,
    height_mm: u32,
    invert: bool,
    copies: u32,
) -> Result<(), String> {
    log_cmd!("imprimir_etiqueta_raw");
    log_info!(
        "parametros: width_mm={}, height_mm={}, invert={}, copies={}, base64_len={}",
        width_mm,
        height_mm,
        invert,
        copies,
        base64_image.len()
    );

    // ── Paso 1: Decodificar Base64 ─────────────────────────────────
    log_info!("[1/6] decodificando imagen Base64...");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&base64_image)
        .map_err(|e| {
            log_error!("fallo al decodificar Base64: {}", e);
            format!("Error decodificando Base64: {}", e)
        })?;
    log_info!("[1/6] Base64 decodificado: {} bytes", decoded.len());

    // ── Paso 2: Leer formato de imagen ─────────────────────────────
    log_info!("[2/6] leyendo formato de imagen...");
    let img = ImageReader::new(Cursor::new(decoded))
        .with_guessed_format()
        .map_err(|e| {
            log_error!("fallo al leer formato: {}", e);
            format!("Error leyendo formato: {}", e)
        })?
        .decode()
        .map_err(|e| {
            log_error!("fallo al decodificar PNG: {}", e);
            format!("Error decodificando PNG: {}", e)
        })?;
    log_info!(
        "[2/6] imagen decodificada: {}x{} pixeles",
        img.width(),
        img.height()
    );

    // ── Paso 3: Redimensionar a dots ───────────────────────────────
    let width_dots = width_mm * 8;
    let height_dots = height_mm * 8;
    let width_bytes = (width_dots as usize + 7) / 8;
    log_info!(
        "[3/6] redimensionando a {}x{} dots ({} bytes/ancho)...",
        width_dots,
        height_dots,
        width_bytes
    );

    let img = img.resize_exact(
        width_dots,
        height_dots,
        image::imageops::FilterType::Lanczos3,
    );

    // ── Paso 4: Convertir a bitmap monocromo ───────────────────────
    log_info!("[4/6] convirtiendo a bitmap monocromo...");
    let gray = img.to_luma8();

    let mut buffer: Vec<u8> = vec![0u8; width_bytes * height_dots as usize];

    for y in 0..height_dots as usize {
        for x_byte in 0..width_bytes {
            let mut byte_val: u8 = 0;
            for bit in 0..8 {
                let x = x_byte * 8 + bit;
                if x < width_dots as usize {
                    let pixel = gray.get_pixel(x as u32, y as u32);
                    let dark = pixel[0] < 128;
                    // La impresora termica imprime 0=negro, 1=blanco.
                    // Invertimos los bits para que el canvas (blanco=fondo, negro=texto)
                    // se imprima correctamente.
                    let set_bit = if invert { dark } else { !dark };
                    if set_bit {
                        byte_val |= 1u8 << (7 - bit);
                    }
                }
            }
            buffer[y * width_bytes + x_byte] = byte_val;
        }
    }
    log_info!(
        "[4/6] bitmap generado: {} bytes totales",
        buffer.len()
    );

    // ── Paso 5: Construir comandos TSPL ────────────────────────────
    log_info!("[5/6] construyendo comandos TSPL...");
    let mut tspl: Vec<u8> = Vec::new();

    write!(
        &mut tspl,
        "CLS\nSIZE {} mm,{} mm\nGAP 0,0\nSPEED 1.5\nDENSITY 15\n",
        width_mm, height_mm
    )
    .map_err(|e| {
        log_error!("fallo al escribir comandos TSPL: {}", e);
        format!("Error TSPL: {}", e)
    })?;

    write!(
        &mut tspl,
        "BITMAP {},{},{},{},0,",
        BITMAP_X_OFFSET, BITMAP_Y_OFFSET, width_bytes, height_dots
    )
    .map_err(|e| {
        log_error!("fallo al escribir comando BITMAP: {}", e);
        format!("Error BITMAP: {}", e)
    })?;

    tspl.extend_from_slice(&buffer);

    let n = if copies == 0 { 1 } else { copies };
    write!(&mut tspl, "\nPRINT {}", n).map_err(|e| {
        log_error!("fallo al escribir comando PRINT: {}", e);
        format!("Error PRINT: {}", e)
    })?;
    log_info!(
        "[5/6] buffer TSPL construido: {} bytes",
        tspl.len()
    );

    // ── Paso 6: Enviar a la impresora ──────────────────────────────
    log_info!("[6/6] enviando a la impresora...");
    let result = write_to_printer(&state, &tspl);
    match &result {
        Ok(()) => log_info!("[6/6] IMPRESION COMPLETADA EXITOSAMENTE"),
        Err(e) => log_error!("[6/6] fallo al imprimir: {}", e),
    }
    result
}

fn write_to_printer(
    state: &PrinterPlugin,
    data: &[u8],
) -> Result<(), String> {
    log_info!(
        "write_to_printer: buscando dispositivo ({} bytes)...",
        data.len()
    );

    #[cfg(target_os = "android")]
    {
        if let Ok(status) = state.bluetooth_status() {
            if status.get("connected").and_then(|c| c.as_bool()).unwrap_or(false) {
                log_info!("write_to_printer: enviando por Bluetooth via plugin...");
                state.bluetooth_print(data).map_err(|e| {
                    log_error!("write_to_printer: error BT plugin: {}", e);
                    format!("Error enviando datos Bluetooth: {}", e)
                })?;
                return Ok(());
            }
        }
        log_error!(
            "write_to_printer: no hay conexion Bluetooth activa en Android. \
             Conecte primero desde la interfaz."
        );
        Err("No se encontro impresora Bluetooth. Conecte primero.".to_string())
    }

    #[cfg(not(target_os = "android"))]
    {
        // ── USB (prioridad) ───────────────────────────────────────────
        for i in 0..10 {
            let usb_path = format!("/dev/usb/lp{}", i);
            if std::path::Path::new(&usb_path).exists() {
                log_info!("write_to_printer: enviando por USB {} ...", usb_path);

                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&usb_path)
                    .map_err(|e| format!("Error abriendo {}: {}", usb_path, e))?;

                file.write_all(data)
                    .map_err(|e| format!("Error escribiendo en {}: {}", usb_path, e))?;

                file.flush()
                    .map_err(|e| format!("Error flush en {}: {}", usb_path, e))?;

                log_info!("write_to_printer: enviado por USB a {}", usb_path);
                return Ok(());
            }
        }

        // ── BLUETOOTH (fallback) ──────────────────────────────────────
        let mac = match LAST_BT_MAC.lock().ok().and_then(|m| m.clone()) {
            Some(m) => m,
            None => match bt_find_connected_printer() {
                Some(m) => {
                    log_info!("write_to_printer: usando impresora BT autodetectada: {}", m);
                    m
                }
                None => {
                    log_error!(
                        "write_to_printer: no hay impresora USB ni MAC Bluetooth. \
                         USB: sudo modprobe usblp. Bluetooth: use bluetooth_connect_printer."
                    );
                    return Err("No se encontro impresora. Verifique USB o Bluetooth.".to_string());
                }
            },
        };

        log_info!("write_to_printer: enviando por Bluetooth (RFCOMM) a {} ...", mac);
        match bt_rfcomm_send(&mac, 1, data) {
            Ok(()) => {
                log_info!("write_to_printer: enviado por Bluetooth a {}", mac);
                Ok(())
            }
            Err(e) => {
                log_error!("write_to_printer: {}", e);
                Err("No se encontro impresora. Verifique USB o Bluetooth.".to_string())
            }
        }
    }
}

// ─── PUNTO DE ENTRADA ──────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    log_info!("╔══════════════════════════════════════════════════════════╗");
    log_info!("║   Sistema de Impresion Termica - Tauri v2               ║");
    log_info!("║   Version: 0.1.0                                        ║");
    log_info!("╚══════════════════════════════════════════════════════════╝");
    log_info!("");
    log_info!("Iniciando aplicacion Tauri...");
    log_info!("");

    let builder = tauri::Builder::default()
        .plugin(plugin_printer::init())
        .invoke_handler(tauri::generate_handler![
            check_printer_status,
            bluetooth_scan_devices,
            bluetooth_connect_printer,
            bluetooth_disconnect_printer,
            imprimir_etiqueta_raw
        ]);

    log_info!("comandos Tauri registrados:");
    log_info!("  - check_printer_status");
    log_info!("  - imprimir_etiqueta_raw");
    log_info!("  - bluetooth_scan_devices");
    log_info!("  - bluetooth_connect_printer");
    log_info!("  - bluetooth_disconnect_printer");
    log_info!("");

    builder
        .setup(|app| {
            log_info!("");
            log_info!("╔══════════════════════════════════════════════════════════╗");
            log_info!("║   SETUP COMPLETADO - Aplicacion lista                    ║");
            log_info!("╚══════════════════════════════════════════════════════════╝");
            log_info!("");

            let handle = app.handle().clone();

            // ── USB Monitor: detecta hotplug sin polling pesado ─────
            // Vigila /dev/usb/ cada 2s. Si cambia, emite evento al frontend.
            std::thread::spawn(move || {
                let mut last_usb: Option<String> = None;
                loop {
                    let current = usblp_device_exists();
                    if current != last_usb {
                        let connected = current.is_some();
                        log_info!(
                            "USB monitor: impresora {} ({}).",
                            if connected { "CONECTADA" } else { "DESCONECTADA" },
                            current.as_deref().unwrap_or("ninguno")
                        );
                        let _ = handle.emit("usb-status", serde_json::json!({
                            "usb_connected": connected,
                            "usb_device": current,
                        }));
                        last_usb = current;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });

            log_info!("USB monitor iniciado. Notificara cambios en /dev/usb/lp*.");
            log_info!("");
            log_info!("Ventana principal creada. Esperando...");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
