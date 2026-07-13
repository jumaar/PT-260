const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ─── DEBUG CONSOLE ───────────────────────────────────────────────────
var debugVisible = true;
var debugLines = [];

function debugLog(msg, cls) {
    cls = cls || '';
    var now = new Date();
    var time = now.getHours().toString().padStart(2,'0') + ':' +
               now.getMinutes().toString().padStart(2,'0') + ':' +
               now.getSeconds().toString().padStart(2,'0');
    console.log('[' + time + ']', msg);
    debugLines.push({ time: time, msg: String(msg), cls: cls });
    if (debugLines.length > 200) debugLines.shift();
    renderDebugLog();
}
function debugErr(msg)  { debugLog(msg, 'log-err'); }
function debugOk(msg)   { debugLog(msg, 'log-ok'); }
function debugWarn(msg) { debugLog(msg, 'log-warn'); }
function debugInfo(msg) { debugLog(msg, 'log-info'); }
function debugInvoke(cmd, args) {
    debugLog('▶ ' + cmd + (args ? ' ' + JSON.stringify(args).substring(0,120) : ''), 'log-invoke');
}

function renderDebugLog() {
    var el = document.getElementById('debug-log');
    if (!el) return;
    var html = '';
    for (var i = 0; i < debugLines.length; i++) {
        var l = debugLines[i];
        html += '<div class="log-line"><span class="log-time">' + l.time + '</span><span class="log-msg ' + l.cls + '">' + esc(l.msg) + '</span></div>';
    }
    el.innerHTML = html;
    el.scrollTop = el.scrollHeight;
}
function esc(s) {
    return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

document.getElementById('debug-tab').addEventListener('click', function() {
    var con = document.getElementById('debug-console');
    var tab = document.getElementById('debug-tab');
    if (debugVisible) {
        con.style.display = 'none';
        tab.textContent = 'LOG';
        tab.classList.remove('hidden');
    } else {
        con.style.display = 'flex';
        tab.textContent = '▼';
    }
    debugVisible = !debugVisible;
});
document.getElementById('debug-toggle').addEventListener('click', function() {
    var con = document.getElementById('debug-console');
    var tab = document.getElementById('debug-tab');
    con.style.display = 'none';
    tab.textContent = 'LOG';
    tab.classList.remove('hidden');
    debugVisible = false;
});
document.getElementById('debug-clear').addEventListener('click', function() {
    debugLines = [];
    renderDebugLog();
});

// ─── LOGS ───────────────────────────────────────────────────────────
function logInfo(msg)  { debugInfo(msg); }
function logWarn(msg)  { debugWarn(msg); }
function logError(msg) { debugErr(msg); }

logInfo('main.js cargado');

// ─── TOAST ──────────────────────────────────────────────────────────
function showToast(msg, isError) {
    debugLog((isError ? 'TOAST ERR: ' : 'TOAST: ') + msg, isError ? 'log-err' : 'log-ok');
    var overlay = document.getElementById('toast-overlay');
    overlay.innerHTML = '';
    var box = document.createElement('div');
    box.id = 'toast-box';
    if (isError) box.className = 'error';
    var p = document.createElement('div');
    p.className = 'toast-msg';
    p.textContent = msg;
    var btn = document.createElement('button');
    btn.className = 'toast-btn';
    btn.textContent = 'OK';
    btn.addEventListener('click', function() { overlay.style.display = 'none'; });
    box.appendChild(p);
    box.appendChild(btn);
    overlay.appendChild(box);
    overlay.style.display = 'flex';
}

// ─── invoke wrapper ──────────────────────────────────────────────────
async function invokeDebug(cmd, args) {
    debugInvoke(cmd, args);
    var started = Date.now();
    try {
        var result = await invoke(cmd, args);
        var elapsed = Date.now() - started;
        debugOk('✔ ' + cmd + ' (' + elapsed + 'ms) → ' + (result !== undefined ? JSON.stringify(result).substring(0,100) : 'void'));
        return result;
    } catch(e) {
        var elapsed = Date.now() - started;
        debugErr('✘ ' + cmd + ' (' + elapsed + 'ms) → ' + String(e));
        throw e;
    }
}

// ─── TECLADO CANVAS (textarea overlay) ───────────────────────────────
var textLines    = [''];
var cursorLine   = 0;
var cursorCol    = 0;
var cursorVisible = true;
var cursorTimer  = null;
var canvasFocused = false;

const keyboardInput = document.getElementById('keyboard-input');
const canvas = document.getElementById('sticker-canvas');

function getCtx() { return canvas.getContext('2d'); }

function syncCanvasToTextarea() {
    keyboardInput.value = textLines.join('\n');
}

function syncTextareaToCanvas() {
    var val = keyboardInput.value;
    var newLines = val.split('\n');
    textLines = newLines;
    var pos = keyboardInput.selectionStart;
    cursorLine = val.substring(0, pos).split('\n').length - 1;
    var lineStart = val.lastIndexOf('\n', pos - 1) + 1;
    cursorCol = pos - lineStart;
    drawPreview();
}

keyboardInput.addEventListener('input', function() {
    syncTextareaToCanvas();
});
keyboardInput.addEventListener('click', function() {
    syncTextareaToCanvas();
    startBlink();
});
keyboardInput.addEventListener('keyup', function() {
    syncTextareaToCanvas();
});

canvas.addEventListener('click', function(e) {
    keyboardInput.focus();
    canvasFocused = true;
    startBlink();
    drawPreview();
    e.preventDefault();
});

canvas.addEventListener('touchstart', function(e) {
    keyboardInput.focus();
    canvasFocused = true;
    startBlink();
    drawPreview();
});

keyboardInput.addEventListener('focus', function() {
    canvasFocused = true;
    startBlink();
    drawPreview();
});
keyboardInput.addEventListener('blur', function() {
    canvasFocused = false;
    stopBlink();
    drawPreview();
});

// ─── PERSISTENCIA ────────────────────────────────────────────────────
const LS_W = 'label_width_mm';
const LS_H = 'label_height_mm';
const LS_F = 'label_fontsize';

function loadNum(key, fallback) {
    try { var v = parseInt(localStorage.getItem(key)); return (v > 0) ? v : fallback; }
    catch(_) { return fallback; }
}
function saveNum(key, val) { try { localStorage.setItem(key, val); } catch(_) {} }

let widthMm  = loadNum(LS_W, 58);
let heightMm = loadNum(LS_H, 40);
let fontSize = loadNum(LS_F, 24);
let usbConnected = false;
let btConnected  = false;
let btDevices    = [];

const PX_PER_MM = 5;

// ─── ELEMENTOS DOM ──────────────────────────────────────────────────
const ctrlWidth       = document.getElementById('ctrl-width');
const ctrlHeight      = document.getElementById('ctrl-height');
const fontSizeInput   = document.getElementById('ctrl-fontsize');
const copiesInput     = document.getElementById('ctrl-copies');
const invertCheckbox  = document.getElementById('invert-checkbox');
const connIcon        = document.getElementById('conn-icon');
const connText        = document.getElementById('conn-text');
const btScanBtn       = document.getElementById('bt-scan-btn');
const btDeviceList    = document.getElementById('bt-device-list');
const btConnectBtn    = document.getElementById('bt-connect-btn');
const btDisconnectBtn = document.getElementById('bt-disconnect-btn');
const btSpinner       = document.getElementById('bt-spinner');

ctrlWidth.value     = widthMm;
ctrlHeight.value    = heightMm;
fontSizeInput.value = fontSize;

// ─── CONTROLES NUMÉRICOS ─────────────────────────────────────────────
function syncWidth(value) {
    widthMm = Math.max(10, Math.min(58, parseInt(value) || 58));
    ctrlWidth.value = widthMm;
    saveNum(LS_W, widthMm);
    updateCanvas();
}
function syncHeight(value) {
    heightMm = Math.max(10, Math.min(100, parseInt(value) || 40));
    ctrlHeight.value = heightMm;
    saveNum(LS_H, heightMm);
    updateCanvas();
}
function syncFontSize(value) {
    fontSize = Math.max(8, Math.min(72, parseInt(value) || 24));
    fontSizeInput.value = fontSize;
    saveNum(LS_F, fontSize);
    drawPreview();
}
ctrlWidth.addEventListener('input',     e => { var v = parseInt(e.target.value); if (!isNaN(v)) { widthMm = v; updateCanvas(); } });
ctrlWidth.addEventListener('change',    e => syncWidth(e.target.value));
ctrlWidth.addEventListener('blur',      e => syncWidth(e.target.value));
ctrlHeight.addEventListener('input',    e => { var v = parseInt(e.target.value); if (!isNaN(v)) { heightMm = v; updateCanvas(); } });
ctrlHeight.addEventListener('change',   e => syncHeight(e.target.value));
ctrlHeight.addEventListener('blur',     e => syncHeight(e.target.value));
fontSizeInput.addEventListener('input', e => { var v = parseInt(e.target.value); if (!isNaN(v)) { fontSize = v; drawPreview(); } });
fontSizeInput.addEventListener('change',e => syncFontSize(e.target.value));
fontSizeInput.addEventListener('blur',  e => syncFontSize(e.target.value));
copiesInput.addEventListener('change',  e => { var v = Math.max(1, Math.min(99, parseInt(e.target.value) || 1)); copiesInput.value = v; });
copiesInput.addEventListener('blur',    e => { var v = Math.max(1, Math.min(99, parseInt(e.target.value) || 1)); copiesInput.value = v; });

document.addEventListener('click', function(e) {
    var btn = e.target.closest('.stepper-btn');
    if (!btn) return;
    var input = document.getElementById(btn.dataset.target);
    if (!input) return;
    var step = parseInt(btn.dataset.step) || 0;
    var min  = parseInt(input.min) || 0;
    var max  = parseInt(input.max) || 100;
    var val  = parseInt(input.value) || min;
    var newVal = Math.max(min, Math.min(max, val + step));
    if (newVal !== val) {
        input.value = newVal;
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.dispatchEvent(new Event('change', { bubbles: true }));
    }
});

// ─── DIBUJO ─────────────────────────────────────────────────────────
function updateCanvas() {
    var w = widthMm * PX_PER_MM, h = heightMm * PX_PER_MM;
    canvas.width  = w * 2;
    canvas.height = h * 2;
    canvas.style.width  = w + 'px';
    canvas.style.height = h + 'px';
    drawPreview();
}

function drawPreview() {
    var ctx = getCtx(), w = canvas.width, h = canvas.height;
    ctx.fillStyle = '#ffffff'; ctx.fillRect(0, 0, w, h);
    ctx.strokeStyle = '#aaaaaa'; ctx.lineWidth = 2; ctx.strokeRect(1, 1, w - 2, h - 2);

    if (!textLines.length || (textLines.length === 1 && textLines[0] === '')) {
        if (canvasFocused) drawCur(ctx, 8, 8);
        else { ctx.textBaseline = 'top'; ctx.fillStyle = '#666'; ctx.font = Math.round(fontSize*2*0.85) + 'px "Segoe UI", sans-serif'; ctx.fillText('Click para escribir', 8, 10); }
        return;
    }
    var fs = Math.round(fontSize * 2), lh = fs * 1.4, pad = 8;
    ctx.fillStyle = '#000'; ctx.font = 'bold ' + fs + 'px "Segoe UI", sans-serif'; ctx.textBaseline = 'top';
    for (var i = 0; i < textLines.length; i++) {
        var y = Math.round(8 + i * lh);
        if (y + fs < h) ctx.fillText(textLines[i], pad, y);
    }
    if (canvasFocused && cursorVisible) {
        ctx.font = 'bold ' + fs + 'px "Segoe UI", sans-serif';
        var txt = (textLines[cursorLine]||'').substring(0, cursorCol);
        var cx = pad + ctx.measureText(txt).width, cy = Math.round(8 + cursorLine * lh);
        drawCur(ctx, cx, cy - 2);
    }
}
function drawCur(ctx, x, y) { ctx.fillStyle = '#000'; ctx.fillRect(x, y, 2, Math.round(fontSize*2*1.2)); }

function startBlink() { stopBlink(); cursorVisible = true; cursorTimer = setInterval(() => { cursorVisible = !cursorVisible; drawPreview(); }, 530); }
function stopBlink()  { if (cursorTimer) { clearInterval(cursorTimer); cursorTimer = null; } cursorVisible = false; }

// ─── PANEL DE CONEXIÓN ──────────────────────────────────────────────
function updateConnUI() {
    btScanBtn.style.display = 'none';
    btDeviceList.style.display = 'none';
    btConnectBtn.style.display = 'none';
    btDisconnectBtn.style.display = 'none';

    if (usbConnected) {
        connIcon.textContent = 'USB';
        connText.textContent = 'USB: Conectado';
        connText.className = 'usb-ok';
        btConnected = false;
    } else if (btConnected) {
        connIcon.textContent = 'BT';
        connText.textContent = 'Bluetooth: Conectado';
        connText.className = 'bt-ok';
        btDisconnectBtn.style.display = 'inline-block';
    } else if (btDevices.length > 0) {
        connIcon.textContent = 'BT';
        connText.textContent = 'USB no detectado. Seleccionar BT:';
        connText.className = 'bt-fail';
        btScanBtn.style.display = 'inline-block';
        btDeviceList.style.display = 'inline-block';
        btConnectBtn.style.display = 'inline-block';
    } else {
        connIcon.textContent = '✗';
        connText.textContent = 'USB: No detectado';
        connText.className = 'usb-fail';
        btScanBtn.style.display = 'inline-block';
        btScanBtn.textContent = 'Escanear BT';
    }
    debugInfo('UI → usb=' + usbConnected + ' bt=' + btConnected + ' devices=' + btDevices.length);
}

// ─── USB LISTENER ────────────────────────────────────────────────────
async function setupUsbListener() {
    await listen('usb-status', function(event) {
        usbConnected = event.payload.usb_connected;
        debugInfo('Evento usb-status: connected=' + usbConnected + ' device=' + event.payload.usb_device);
        updateConnUI();
    });
}

// ─── BLUETOOTH ──────────────────────────────────────────────────────
async function btScan() {
    logInfo('Escaneando Bluetooth...');
    connText.textContent = 'Buscando dispositivos...'; connText.className = '';
    btScanBtn.disabled = true;
    btScanBtn.style.display = 'none';
    btSpinner.style.display = 'inline-block';
    debugLog('=== INICIANDO ESCANEO BLUETOOTH ===', 'log-info');

    try {
        debugLog('Solicitando permisos Bluetooth...', 'log-info');
        await invokeDebug('bluetooth_request_permissions');
    } catch(e) {
        debugWarn('Permisos BT (puede ya estar concedido): ' + e);
    }

    try {
        var result = await invokeDebug('bluetooth_scan_devices', { timeoutSecs: 10 });
        btDevices = result.devices || [];

        // Mostrar diagnostico si existe
        if (result._diag) {
            var d = result._diag;
            debugInfo('BT DIAG: adapter=' + d.adapter_available +
                      ' enabled=' + d.adapter_enabled +
                      ' bonded=' + d.bonded_device_count +
                      ' name=' + d.adapter_name +
                      ' addr=' + d.adapter_address +
                      ' error=' + (d.error || 'none'));
        }
        if (result._error) {
            debugErr('BT SCAN ERROR: ' + result._error);
        }

        btDeviceList.innerHTML = '<option value="" style="color:#000">-- Seleccionar --</option>';
        btDevices.forEach(d => {
            var o = document.createElement('option');
            o.value = d.mac;
            o.textContent = d.name + ' (' + d.mac + ')';
            o.style.color = '#000';
            btDeviceList.appendChild(o);
        });
        debugOk('BT: ' + btDevices.length + ' dispositivos encontrados');
        connText.textContent = btDevices.length + ' dispositivo(s) encontrado(s)';
    } catch(e) {
        debugErr('BT scan error: ' + e);
        connText.textContent = 'Error al escanear'; connText.className = 'bt-fail';
    }

    btSpinner.style.display = 'none';
    btScanBtn.disabled = false;
    btScanBtn.style.display = 'inline-block';
    updateConnUI();
}

async function btConnect() {
    var mac = btDeviceList.value; if (!mac) return;
    debugLog('=== INICIANDO CONEXION BLUETOOTH a ' + mac + ' ===', 'log-info');
    connText.textContent = 'Conectando BT...';
    btConnectBtn.disabled = true;

    try {
        debugLog('Solicitando permisos Bluetooth...', 'log-info');
        await invokeDebug('bluetooth_request_permissions');
    } catch(e) {
        debugWarn('Permisos BT (puede ya estar concedido): ' + e);
    }

    try {
        var result = await invokeDebug('bluetooth_connect_printer', { mac: mac });
        btConnected = true;
        debugOk('BT conectado a ' + mac);
    } catch(e) {
        debugErr('BT connect error: ' + e);
        connText.textContent = 'Error: ' + String(e).substring(0, 60);
        connText.className = 'bt-fail';
        btConnected = false;
    }
    btConnectBtn.disabled = false;
    updateConnUI();
}

async function btDisconnect() {
    debugLog('=== DESCONECTANDO BLUETOOTH ===', 'log-info');
    try { await invokeDebug('bluetooth_disconnect_printer'); } catch(e) { debugErr('BT disconnect error: ' + e); }
    btConnected = false; btDevices = []; btDeviceList.innerHTML = '';
    updateConnUI();
}

btScanBtn.addEventListener('click', btScan);
btConnectBtn.addEventListener('click', btConnect);
btDisconnectBtn.addEventListener('click', btDisconnect);

// ─── IMPRESIÓN ──────────────────────────────────────────────────────
async function ejecutarImpresion() {
    debugLog('=== IMPRIMIENDO ===', 'log-info');
    var wasFocused = canvasFocused; canvasFocused = false; cursorVisible = false;
    drawPreview();
    var b64 = canvas.toDataURL('image/png').split(',')[1];
    canvasFocused = wasFocused; if (canvasFocused) startBlink(); drawPreview();

    var copies = parseInt(copiesInput.value) || 1;
    if (copies < 1) copies = 1;
    if (copies > 99) copies = 99;
    copiesInput.value = copies;

    try {
        await invokeDebug('imprimir_etiqueta_raw', { base64Image: b64, widthMm: widthMm, heightMm: heightMm, invert: invertCheckbox.checked, copies: copies });
        debugOk('Impresion OK');
        showToast('Impresión enviada');
    } catch(e) {
        debugErr('Impresion FAIL: ' + e);
        showToast('Error al imprimir. Verifique la conexion.', true);
    }
}

// ─── BOTONES ────────────────────────────────────────────────────────
document.getElementById('btn-clear').addEventListener('click', () => {
    textLines = [''];
    cursorLine = 0; cursorCol = 0;
    syncCanvasToTextarea();
    drawPreview();
    keyboardInput.focus();
});
document.getElementById('btn-print').addEventListener('click', ejecutarImpresion);

// ─── INICIO ─────────────────────────────────────────────────────────
async function init() {
    copiesInput.value = 1;
    updateCanvas();
    syncCanvasToTextarea();
    setupUsbListener();

    // Listener para diagnosticos Bluetooth emitidos desde Rust
    await listen('bt-scan-diag', function(event) {
        var p = event.payload;
        debugInfo('BT DIAG: adapter=' + p.adapter_available + ' enabled=' + p.adapter_enabled +
                  ' bonded=' + p.bonded_device_count + ' name=' + p.adapter_name +
                  ' addr=' + p.adapter_address + ' error=' + (p.error || 'none'));
    });

    debugLog('=== INICIANDO APP ===', 'log-info');
    connText.textContent = 'Verificando conexiones...';
    connText.className = 'bt-reconnect';
    try {
        var s = await invokeDebug('check_printer_status');
        usbConnected = s.usb_connected;
        btConnected  = s.bt_connected;
        debugInfo('Estado inicial: USB=' + usbConnected + ' BT=' + btConnected + ' hw=' + s.hw_present + ' type=' + s.connection_type + ' btDevice=' + s.bt_device);
        if (btConnected && s.bt_device) {
            debugInfo('BT auto-reconectado a: ' + s.bt_device);
        }
    } catch(e) {
        debugErr('No se pudo consultar estado inicial: ' + e);
    }
    updateConnUI();
}
init();
