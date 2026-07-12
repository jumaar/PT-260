use base64::Engine;
use image::ImageReader;
use std::io::{Cursor, Write};
use std::process::Command;
use std::sync::Mutex;
use tauri::Emitter;

// MAC del último dispositivo Bluetooth conectado (para reconexión automática)
static LAST_BT_MAC: Mutex<Option<String>> = Mutex::new(None);

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

// ─── DETECCIÓN DE HARDWARE ─────────────────────────────────────────
// IDs de la impresora GEZHI (mismos que en el sistema Python)
const IMPRESORA_VID: &str = "0483";
const IMPRESORA_PID: &str = "5720";
// Offset horizontal para el comando BITMAP (idéntico al X_OFFSET de Python)
const BITMAP_X_OFFSET: i32 = -30;

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

/// Conecta al dispositivo Bluetooth por SPP (RFCOMM) y devuelve la ruta
/// del dispositivo serie (/dev/rfcommX).
fn bluetooth_connect_rfcomm(mac: &str, channel: u8) -> Result<String, String> {
    // Primero liberar cualquier rfcomm previo
    for i in 0..5 {
        let path = format!("/dev/rfcomm{}", i);
        if std::path::Path::new(&path).exists() {
            log_info!("bluetooth: liberando {} ...", path);
            let _ = Command::new("rfcomm")
                .args(["release", &i.to_string()])
                .output();
        }
    }

    log_info!(
        "bluetooth: conectando rfcomm {} a {} canal {} ...",
        0,
        mac,
        channel
    );

    // Intentar rfcomm bind sin privilegios primero
    let rfcomm_path = "/dev/rfcomm0".to_string();
    let bind_result = Command::new("rfcomm")
        .args(["bind", "0", mac, &channel.to_string()])
        .output();

    match bind_result {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                log_info!("bluetooth: rfcomm: {}", stderr.trim());
            }
            if std::path::Path::new(&rfcomm_path).exists() {
                log_info!("bluetooth: dispositivo RFCOMM creado: {}", rfcomm_path);
                // Configurar en modo raw para evitar que el TTY corrompa datos binarios
                let _ = Command::new("stty")
                    .args(["-F", &rfcomm_path, "raw", "-echo", "-echoe", "-echok"])
                    .output();
                return Ok(rfcomm_path);
            }
        }
        Err(_) => {
            log_info!("bluetooth: rfcomm bind falló, intentando con pkexec...");
        }
    }

    // Intentar con pkexec (GUI, no requiere terminal)
    log_info!("bluetooth: solicitando permisos via pkexec...");
    let pkexec_result = Command::new("pkexec")
        .args(["rfcomm", "bind", "0", mac, &channel.to_string()])
        .output();

    match pkexec_result {
        Ok(output) => {
            if std::path::Path::new(&rfcomm_path).exists() {
                log_info!("bluetooth: dispositivo RFCOMM creado via pkexec: {}", rfcomm_path);
                return Ok(rfcomm_path);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            log_error!("bluetooth: pkexec rfcomm falló: {}", stderr.trim());
        }
        Err(e) => {
            log_error!("bluetooth: pkexec falló: {}", e);
        }
    }

    // Si todo falla, dar instrucciones claras
    Err(format!(
        "No se pudo crear {}. Solución única:
         sudo usermod -a -G dialout $USER && reinicie sesión.
         Luego reintente conectar.",
        rfcomm_path
    ))
}

/// Verifica si hay dispositivos Bluetooth RFCOMM disponibles.
fn bluetooth_device_exists() -> Option<String> {
    for i in 0..5 {
        let path = format!("/dev/rfcomm{}", i);
        if std::path::Path::new(&path).exists() {
            log_info!("bluetooth: dispositivo RFCOMM encontrado: {}", path);
            return Some(path);
        }
    }
    None
}
// ─── COMANDOS TAURI ────────────────────────────────────────────────

#[tauri::command]
fn check_printer_status() -> serde_json::Value {
    log_cmd!("check_printer_status");

    let hw_present = usb_device_present(IMPRESORA_VID, IMPRESORA_PID);
    let bt_device = bluetooth_device_exists();
    let lp_device = usblp_device_exists();
    let connected = (hw_present && lp_device.is_some()) || bt_device.is_some();

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

    if connected {
        log_info!("check_printer_status → IMPRESORA CONECTADA y lista");
    } else {
        log_warn!("check_printer_status → IMPRESORA NO LISTA");
    }

    serde_json::json!({
        "connected": connected,
        "hw_present": hw_present,
        "lp_device": lp_device,
        "bt_device": bt_device,
        "connection_type": if bt_device.is_some() { "bluetooth" } else if hw_present && lp_device.is_some() { "usb" } else { "none" }
    })
}

// ─── COMANDOS BLUETOOTH ─────────────────────────────────────────────

#[tauri::command]
fn bluetooth_scan_devices(timeout_secs: Option<u64>) -> Vec<serde_json::Value> {
    log_cmd!("bluetooth_scan_devices");
    let timeout = timeout_secs.unwrap_or(10);
    let devices = bluetooth_scan(timeout);
    devices
        .into_iter()
        .map(|(mac, name)| {
            serde_json::json!({"mac": mac, "name": name})
        })
        .collect()
}

#[tauri::command]
fn bluetooth_connect_printer(mac: String) -> Result<String, String> {
    log_cmd!("bluetooth_connect_printer");
    log_info!("bluetooth: solicitada conexión a {}", mac);

    // Emparejar primero
    bluetooth_pair(&mac)?;

    // Guardar MAC para reconexión automática
    if let Ok(mut stored) = LAST_BT_MAC.lock() {
        *stored = Some(mac.clone());
    }

    // Conectar RFCOMM canal 1 (SPP estándar para impresoras térmicas)
    let device_path = bluetooth_connect_rfcomm(&mac, 1)?;
    log_info!("bluetooth: impresora conectada en {}", device_path);
    Ok(device_path)
}

#[tauri::command]
fn bluetooth_disconnect_printer() -> Result<(), String> {
    log_cmd!("bluetooth_disconnect_printer");

    for i in 0..5 {
        let path = format!("/dev/rfcomm{}", i);
        if std::path::Path::new(&path).exists() {
            log_info!("bluetooth: liberando {} ...", path);
            let output = Command::new("rfcomm")
                .args(["release", &i.to_string()])
                .output()
                .map_err(|e| format!("Error liberando {}: {}", path, e))?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            log_info!("bluetooth: rfcomm release: {}", stderr.trim());
        }
    }

    // Limpiar MAC almacenada
    if let Ok(mut stored) = LAST_BT_MAC.lock() {
        *stored = None;
    }
    log_info!("bluetooth: desconectado");
    Ok(())
}


#[tauri::command]
fn imprimir_etiqueta_raw(
    base64_image: String,
    width_mm: u32,
    height_mm: u32,
    invert: bool,
) -> Result<(), String> {
    log_cmd!("imprimir_etiqueta_raw");
    log_info!(
        "parametros: width_mm={}, height_mm={}, invert={}, base64_len={}",
        width_mm,
        height_mm,
        invert,
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
        "CLS\nSIZE {} mm,{} mm\nGAP 0,0\nDENSITY 15\n",
        width_mm, height_mm
    )
    .map_err(|e| {
        log_error!("fallo al escribir comandos TSPL: {}", e);
        format!("Error TSPL: {}", e)
    })?;

    write!(
        &mut tspl,
        "BITMAP {},0,{},{},0,",
        BITMAP_X_OFFSET, width_bytes, height_dots
    )
    .map_err(|e| {
        log_error!("fallo al escribir comando BITMAP: {}", e);
        format!("Error BITMAP: {}", e)
    })?;

    tspl.extend_from_slice(&buffer);

    write!(&mut tspl, "\nPRINT 1\n").map_err(|e| {
        log_error!("fallo al escribir comando PRINT: {}", e);
        format!("Error PRINT: {}", e)
    })?;
    log_info!(
        "[5/6] buffer TSPL construido: {} bytes",
        tspl.len()
    );

    // ── Paso 6: Enviar a la impresora ──────────────────────────────
    log_info!("[6/6] enviando a la impresora...");
    let result = write_to_printer(&tspl);
    match &result {
        Ok(()) => log_info!("[6/6] IMPRESION COMPLETADA EXITOSAMENTE"),
        Err(e) => log_error!("[6/6] fallo al imprimir: {}", e),
    }
    result
}

fn write_to_printer(data: &[u8]) -> Result<(), String> {
    log_info!(
        "write_to_printer: buscando dispositivo ({} bytes)...",
        data.len()
    );

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
    // Si /dev/rfcomm0 existe pero da EBUSY, el enlace RFCOMM se cayó.
    // Solución: liberar, re-vincular, y reintentar.
    let mac_for_rebind = LAST_BT_MAC.lock().ok()
        .and_then(|m| m.clone());

    for attempt in 0..2 {
        let bt_path = bluetooth_device_exists();
        if bt_path.is_none() && attempt == 0 && mac_for_rebind.is_some() {
            // No hay dispositivo. Reconectar desde cero.
            let mac = mac_for_rebind.as_ref().unwrap();
            log_info!("write_to_printer: reconectando BT a {}...", mac);
            let _ = bluetooth_pair(mac);
            let _ = bluetooth_connect_rfcomm(mac, 1);
        }

        if let Some(ref path) = bluetooth_device_exists() {
            log_info!("write_to_printer: intentando Bluetooth {} (intento {})...", path, attempt + 1);
            let _ = Command::new("stty").args(["-F", path, "raw", "-echo"]).output();

            match std::fs::OpenOptions::new().write(true).open(path) {
                Ok(mut file) => {
                    file.write_all(data)
                        .map_err(|e| format!("Error escribiendo en BT: {}", e))?;
                    let _ = file.flush();
                    log_info!("write_to_printer: enviado por Bluetooth a {}", path);
                    return Ok(());
                }
                Err(e) => {
                    let err_msg = format!("{}", e);
                    log_warn!("write_to_printer: error abriendo {}: {}", path, err_msg);
                    // Si es EBUSY o similar, liberar y re-vincular para el 2do intento
                    if attempt == 0 && mac_for_rebind.is_some() {
                        let mac = mac_for_rebind.as_ref().unwrap();
                        log_info!("write_to_printer: liberando y re-vinculando rfcomm para {}...", mac);
                        let _ = Command::new("rfcomm").args(["release", "0"]).output();
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let _ = bluetooth_connect_rfcomm(mac, 1);
                    }
                }
            }
        } else {
            break; // no hay dispositivo, no reintentar
        }
    }

    log_error!(
        "write_to_printer: no se encontro dispositivo.          USB: sudo modprobe usblp. Bluetooth: use bluetooth_connect_printer."
    );
    Err("No se encontro impresora. Verifique USB o Bluetooth.".to_string())
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
