const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ─── LOGS ───────────────────────────────────────────────────────────
function logInfo(msg)  { console.log('[front]', msg); }
function logWarn(msg)  { console.warn('[front]', msg); }
function logError(msg) { console.error('[front]', msg); }

logInfo('main.js cargado');

// ─── TOAST (reemplaza alert nativo que muestra URL en la cabecera) ──
function showToast(msg, isError) {
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

// ─── PERSISTENCIA (localStorage) ────────────────────────────────────
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
const canvas          = document.getElementById('sticker-canvas');
const invertCheckbox  = document.getElementById('invert-checkbox');
const connIcon        = document.getElementById('conn-icon');
const connText        = document.getElementById('conn-text');
const btScanBtn       = document.getElementById('bt-scan-btn');
const btDeviceList    = document.getElementById('bt-device-list');
const btConnectBtn    = document.getElementById('bt-connect-btn');
const btDisconnectBtn = document.getElementById('bt-disconnect-btn');
const btSpinner       = document.getElementById('bt-spinner');

// Inicializar controles con valores persistidos
ctrlWidth.value     = widthMm;
ctrlHeight.value    = heightMm;
fontSizeInput.value = fontSize;

// ─── TEXTO SOBRE CANVAS ─────────────────────────────────────────────
let textLines    = [''];
let cursorLine   = 0;
let cursorCol    = 0;
let cursorVisible = true;
let cursorTimer  = null;
let canvasFocused = false;

function getCtx() { return canvas.getContext('2d'); }

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
ctrlWidth.addEventListener('input',     e => syncWidth(e.target.value));
ctrlHeight.addEventListener('input',    e => syncHeight(e.target.value));
fontSizeInput.addEventListener('input', e => syncFontSize(e.target.value));

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

// ─── TECLADO CANVAS ─────────────────────────────────────────────────
canvas.addEventListener('keydown', function(e) {
    if (!canvasFocused) return;
    var h = true;
    if (e.key === 'Enter') {
        var cur = textLines[cursorLine]||''; textLines[cursorLine] = cur.substring(0, cursorCol);
        textLines.splice(cursorLine+1, 0, cur.substring(cursorCol)); cursorLine++; cursorCol = 0;
    } else if (e.key === 'Backspace') {
        if (cursorCol > 0) { var ln = textLines[cursorLine]||''; textLines[cursorLine] = ln.substring(0,cursorCol-1)+ln.substring(cursorCol); cursorCol--; }
        else if (cursorLine > 0) { var pl = (textLines[cursorLine-1]||'').length; textLines[cursorLine-1] += (textLines[cursorLine]||''); textLines.splice(cursorLine,1); cursorLine--; cursorCol = pl; }
    } else if (e.key === 'ArrowLeft')  { if (cursorCol>0) cursorCol--; else if (cursorLine>0) { cursorLine--; cursorCol = (textLines[cursorLine]||'').length; } }
    else if (e.key === 'ArrowRight') { var ln = (textLines[cursorLine]||'').length; if (cursorCol<ln) cursorCol++; else if (cursorLine<textLines.length-1) { cursorLine++; cursorCol=0; } }
    else if (e.key === 'ArrowUp')    { if (cursorLine>0) { cursorLine--; var l=(textLines[cursorLine]||'').length; if (cursorCol>l) cursorCol=l; } }
    else if (e.key === 'ArrowDown')  { if (cursorLine<textLines.length-1) { cursorLine++; var l=(textLines[cursorLine]||'').length; if (cursorCol>l) cursorCol=l; } }
    else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) { var ln = textLines[cursorLine]||''; textLines[cursorLine] = ln.substring(0,cursorCol)+e.key+ln.substring(cursorCol); cursorCol++; }
    else h = false;
    if (h) { e.preventDefault(); startBlink(); drawPreview(); }
});
canvas.addEventListener('focus', () => { canvasFocused = true; startBlink(); drawPreview(); });
canvas.addEventListener('blur',  () => { canvasFocused = false; stopBlink(); drawPreview(); });
canvas.addEventListener('click', () => canvas.focus());

// ─── PANEL DE CONEXIÓN (USB prioritario, BT manual) ─────────────────
function updateConnUI() {
    // Ocultar todo primero
    btScanBtn.style.display = 'none';
    btDeviceList.style.display = 'none';
    btConnectBtn.style.display = 'none';
    btDisconnectBtn.style.display = 'none';

    if (usbConnected) {
        // USB activo: no mostrar Bluetooth
        connIcon.textContent = '🔌';
        connText.textContent = 'USB: Conectado';
        connText.className = 'usb-ok';
        btConnected = false;
    } else if (btConnected) {
        // Bluetooth conectado manualmente
        connIcon.textContent = '📶';
        connText.textContent = 'Bluetooth: Conectado';
        connText.className = 'bt-ok';
        btDisconnectBtn.style.display = 'inline-block';
    } else if (btDevices.length > 0) {
        // Bluetooth escaneado, lista disponible
        connIcon.textContent = '📶';
        connText.textContent = 'USB no detectado. Seleccionar BT:';
        connText.className = 'bt-fail';
        btScanBtn.style.display = 'inline-block';
        btDeviceList.style.display = 'inline-block';
        btConnectBtn.style.display = 'inline-block';
    } else {
        // Sin USB, sin BT escaneado
        connIcon.textContent = '❌';
        connText.textContent = 'USB: No detectado';
        connText.className = 'usb-fail';
        btScanBtn.style.display = 'inline-block';
        btScanBtn.textContent = 'Escanear BT';
    }
}

// ─── EVENTOS USB DESDE RUST ─────────────────────────────────────────
async function setupUsbListener() {
    await listen('usb-status', function(event) {
        usbConnected = event.payload.usb_connected;
        logInfo('Evento usb-status: connected=' + usbConnected + ' device=' + event.payload.usb_device);
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

    try {
        var result = await invoke('bluetooth_scan_devices', { timeoutSecs: 10 });
        btDevices = result;
        btDeviceList.innerHTML = '<option value="" style="color:#000">-- Seleccionar --</option>';
        result.forEach(d => {
            var o = document.createElement('option');
            o.value = d.mac;
            o.textContent = d.name + ' (' + d.mac + ')';
            o.style.color = '#000';
            btDeviceList.appendChild(o);
        });
        logInfo('BT: ' + result.length + ' dispositivos');
        connText.textContent = result.length + ' dispositivo(s) encontrado(s)';
    } catch(e) {
        logError('BT scan error: ' + e);
        connText.textContent = 'Error al escanear BT'; connText.className = 'bt-fail';
    }

    btSpinner.style.display = 'none';
    btScanBtn.disabled = false;
    btScanBtn.style.display = 'inline-block';
    updateConnUI();
}

async function btConnect() {
    var mac = btDeviceList.value; if (!mac) return;
    logInfo('Conectando BT: ' + mac);
    connText.textContent = 'Conectando BT...';
    try {
        await invoke('bluetooth_connect_printer', { mac: mac });
        btConnected = true;
        logInfo('BT conectado');
    } catch(e) { logError('BT connect error: ' + e); connText.textContent = 'Error: ' + e; connText.className = 'bt-fail'; btConnected = false; }
    updateConnUI();
}

async function btDisconnect() {
    try { await invoke('bluetooth_disconnect_printer'); } catch(_) {}
    btConnected = false; btDevices = []; btDeviceList.innerHTML = '';
    updateConnUI();
}

btScanBtn.addEventListener('click', btScan);
btConnectBtn.addEventListener('click', btConnect);
btDisconnectBtn.addEventListener('click', btDisconnect);

// ─── IMPRESIÓN ──────────────────────────────────────────────────────
async function ejecutarImpresion() {
    logInfo('=== IMPRIMIENDO ===');
    var wasFocused = canvasFocused; canvasFocused = false; cursorVisible = false;
    drawPreview();
    var b64 = canvas.toDataURL('image/png').split(',')[1];
    canvasFocused = wasFocused; if (canvasFocused) startBlink(); drawPreview();

    var copies = parseInt(copiesInput.value) || 1;
    if (copies < 1) copies = 1;
    if (copies > 99) copies = 99;
    copiesInput.value = copies;

    try {
        await invoke('imprimir_etiqueta_raw', { base64Image: b64, widthMm: widthMm, heightMm: heightMm, invert: invertCheckbox.checked, copies: copies });
        logInfo('impresion OK');
        showToast('Impresión enviada');
    } catch(e) { logError('impresion FAIL: ' + e); showToast('Error al imprimir. Verifique la conexion.', true); }
}

// ─── BOTONES ────────────────────────────────────────────────────────
document.getElementById('btn-clear').addEventListener('click', () => { textLines = ['']; cursorLine = 0; cursorCol = 0; drawPreview(); canvas.focus(); });
document.getElementById('btn-print').addEventListener('click', ejecutarImpresion);

// ─── INICIO ─────────────────────────────────────────────────────────
async function init() {
    copiesInput.value = 1;
    updateCanvas();
    setupUsbListener();

    // Consulta inicial: el backend dice el estado real
    // (incluye intento de reconexión automática a MACs Bluetooth persistidas)
    connText.textContent = 'Verificando conexiones...';
    connText.className = 'bt-reconnect';
    try {
        var s = await invoke('check_printer_status');
        usbConnected = s.usb_connected;
        btConnected  = s.bt_connected;
        logInfo('Estado inicial: USB=' + usbConnected + ' BT=' + btConnected);
        if (btConnected && s.bt_device) {
            logInfo('BT auto-reconectado a: ' + s.bt_device);
        }
    } catch(_) {
        logWarn('No se pudo consultar estado inicial');
    }
    updateConnUI();
}
init();
