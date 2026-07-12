package com.sistema_impresion.app

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbConstants
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbEndpoint
import android.hardware.usb.UsbManager
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import app.tauri.plugin.Invoke

@TauriPlugin
class PrinterPlugin(private val context: Context) : Plugin(context) {

    companion object {
        private const val ACTION_USB_PERMISSION = "com.sistema_impresion.app.USB_PERMISSION"
    }

    private var permissionCallback: Invoke? = null

    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (ACTION_USB_PERMISSION == intent.action) {
                synchronized(this) {
                    val device = intent.getParcelableExtra<UsbDevice>(UsbManager.EXTRA_DEVICE)
                    if (intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false) && device != null) {
                        permissionCallback?.let { cb ->
                            sendPrintData(cb)
                        }
                    } else {
                        permissionCallback?.reject("Permiso USB denegado por el usuario")
                    }
                    permissionCallback = null
                }
            }
        }
    }

    init {
        val filter = IntentFilter(ACTION_USB_PERMISSION)
        context.registerReceiver(usbReceiver, filter)
    }

    override fun onDestroy() {
        context.unregisterReceiver(usbReceiver)
        super.onDestroy()
    }

    private fun findPrinterDevice(manager: UsbManager): UsbDevice? {
        for (device in manager.deviceList.values) {
            if (device.deviceClass == UsbConstants.USB_CLASS_PRINTER) return device
            for (i in 0 until device.interfaceCount) {
                if (device.getInterface(i).interfaceClass == UsbConstants.USB_CLASS_PRINTER) {
                    return device
                }
            }
        }
        return null
    }

    private fun findBulkOutEndpoint(device: UsbDevice): UsbEndpoint? {
        val intf = device.getInterface(0)
        for (i in 0 until intf.endpointCount) {
            val ep = intf.getEndpoint(i)
            if (ep.type == UsbConstants.USB_ENDPOINT_XFER_BULK &&
                ep.direction == UsbConstants.USB_DIR_OUT
            ) {
                return ep
            }
        }
        return null
    }

    @Command
    fun getPrinterStatus(invoke: Invoke) {
        val manager = context.getSystemService(Context.USB_SERVICE) as UsbManager
        val connected = findPrinterDevice(manager) != null
        val response = JSObject()
        response.put("connected", connected)
        invoke.resolve(response)
    }

    private fun sendPrintData(invoke: Invoke) {
        val bytes = invoke.getArray("data")?.let { jsonArray ->
            ByteArray(jsonArray.length()) { i -> jsonArray.getInt(i).toByte() }
        } ?: run {
            invoke.reject("No se proporcionaron bytes de impresión")
            return
        }

        val manager = context.getSystemService(Context.USB_SERVICE) as UsbManager
        val printerDevice = findPrinterDevice(manager) ?: run {
            invoke.reject("No se encontró impresora USB conectada")
            return
        }

        if (!manager.hasPermission(printerDevice)) {
            permissionCallback = invoke
            val permissionIntent = PendingIntent.getBroadcast(
                context,
                0,
                Intent(ACTION_USB_PERMISSION),
                PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
            manager.requestPermission(printerDevice, permissionIntent)
            return
        }

        val connection = manager.openDevice(printerDevice) ?: run {
            invoke.reject("Error al abrir conexión con el dispositivo USB")
            return
        }

        val endpoint = findBulkOutEndpoint(printerDevice) ?: run {
            connection.close()
            invoke.reject("No se encontró Endpoint de salida bulk para escritura")
            return
        }

        val intf = printerDevice.getInterface(0)
        connection.claimInterface(intf, true)

        val result = connection.bulkTransfer(endpoint, bytes, bytes.size, 5000)

        connection.releaseInterface(intf)
        connection.close()

        if (result >= 0) {
            invoke.resolve()
        } else {
            invoke.reject("Fallo en la transferencia de datos USB. Código: $result")
        }
    }

    @Command
    fun printData(invoke: Invoke) {
        sendPrintData(invoke)
    }

    @Command
    fun checkPrinterStatus(invoke: Invoke) {
        getPrinterStatus(invoke)
    }
    // ================================================================
    // BLUETOOTH SPP
    // ================================================================

    private var btSocket: android.bluetooth.BluetoothSocket? = null
    private var btOutputStream: java.io.OutputStream? = null
    private val SPP_UUID = java.util.UUID.fromString("00001101-0000-1000-8000-00805F9B34FB")

    @Command
    fun bluetoothScan(invoke: Invoke) {
        val adapter = android.bluetooth.BluetoothAdapter.getDefaultAdapter()
        if (adapter == null || !adapter.isEnabled) {
            invoke.reject("Bluetooth no disponible o desactivado")
            return
        }

        val devices = adapter.bondedDevices
        val list = org.json.JSONArray()
        for (d in devices) {
            val obj = org.json.JSONObject()
            obj.put("mac", d.address)
            obj.put("name", d.name ?: "Desconocido")
            list.put(obj)
        }

        val resp = JSObject()
        resp.put("devices", list)
        invoke.resolve(resp)
    }

    @Command
    fun bluetoothConnect(invoke: Invoke) {
        val mac = invoke.parseJSObject().getString("mac") ?: run {
            invoke.reject("Direccion MAC requerida")
            return
        }

        val adapter = android.bluetooth.BluetoothAdapter.getDefaultAdapter()
        val device = adapter?.getRemoteDevice(mac) ?: run {
            invoke.reject("Dispositivo no encontrado: $mac")
            return
        }

        try {
            btSocket?.close()
            btSocket = device.createRfcommSocketToServiceRecord(SPP_UUID)
            btSocket?.connect()
            btOutputStream = btSocket?.outputStream

            val resp = JSObject()
            resp.put("connected", true)
            resp.put("mac", mac)
            invoke.resolve(resp)
        } catch (e: Exception) {
            invoke.reject("Error conectando Bluetooth: ${e.message}")
        }
    }

    @Command
    fun bluetoothDisconnect(invoke: Invoke) {
        try {
            btOutputStream?.close()
            btSocket?.close()
        } catch (_: Exception) {}
        btOutputStream = null
        btSocket = null
        invoke.resolve()
    }

    @Command
    fun bluetoothPrint(invoke: Invoke) {
        val bytes = invoke.getArray("data")?.let { arr ->
            ByteArray(arr.length()) { i -> arr.getInt(i).toByte() }
        } ?: run {
            invoke.reject("No se proporcionaron datos")
            return
        }

        val out = btOutputStream ?: run {
            invoke.reject("No hay conexion Bluetooth activa")
            return
        }

        try {
            out.write(bytes)
            out.flush()
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("Error enviando datos Bluetooth: ${e.message}")
        }
    }

    @Command
    fun bluetoothStatus(invoke: Invoke) {
        val resp = JSObject()
        resp.put("connected", btSocket?.isConnected == true)
        invoke.resolve(resp)
    }
}
