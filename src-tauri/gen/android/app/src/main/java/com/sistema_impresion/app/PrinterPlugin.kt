package com.sistema_impresion.app

import android.Manifest
import android.app.Activity
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
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import app.tauri.plugin.Invoke

@TauriPlugin(
    permissions = [
        Permission(
            strings = [
                Manifest.permission.BLUETOOTH_CONNECT,
                Manifest.permission.BLUETOOTH_SCAN,
            ],
            alias = "bluetooth"
        )
    ]
)
class PrinterPlugin(private val context: Activity) : Plugin(context) {

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
        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(usbReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            context.registerReceiver(usbReceiver, filter)
        }
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
        val bytes = invoke.getArgs().optJSONArray("data")?.let { jsonArray ->
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

    private fun hasBtPermissions(): Boolean {
        if (android.os.Build.VERSION.SDK_INT < android.os.Build.VERSION_CODES.S) return true
        return context.checkSelfPermission(Manifest.permission.BLUETOOTH_SCAN) == android.content.pm.PackageManager.PERMISSION_GRANTED &&
               context.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == android.content.pm.PackageManager.PERMISSION_GRANTED
    }

    @Command
    fun bluetoothScan(invoke: Invoke) {
        try {
            if (!hasBtPermissions()) {
                invoke.reject("Permisos Bluetooth no concedidos. Abra Ajustes > Apps > Sistema Impresion > Permisos y active Bluetooth.")
                return
            }

            val adapter = android.bluetooth.BluetoothAdapter.getDefaultAdapter()

            val resp = JSObject()
            resp.put("adapter_available", adapter != null)
            resp.put("adapter_enabled", adapter?.isEnabled ?: false)
            resp.put("adapter_name", adapter?.name ?: "null")
            resp.put("adapter_address", adapter?.address ?: "null")

            if (adapter == null) {
                resp.put("error", "BluetoothAdapter es null")
                invoke.resolve(resp)
                return
            }
            if (!adapter.isEnabled) {
                resp.put("error", "Bluetooth esta desactivado")
                invoke.resolve(resp)
                return
            }

            val devices = adapter.bondedDevices
            resp.put("bonded_device_count", devices.size)

            val list = org.json.JSONArray()
            for (d in devices) {
                val obj = org.json.JSONObject()
                obj.put("mac", d.address)
                obj.put("name", d.name ?: "Desconocido")
                list.put(obj)
            }

            resp.put("devices", list)
            invoke.resolve(resp)
        } catch (e: SecurityException) {
            val resp = JSObject()
            resp.put("error", "SecurityException: ${e.message}")
            resp.put("devices", org.json.JSONArray())
            invoke.resolve(resp)
        } catch (e: Exception) {
            val resp = JSObject()
            resp.put("error", "Exception: ${e.message}")
            resp.put("devices", org.json.JSONArray())
            invoke.resolve(resp)
        }
    }

    @Command
    fun bluetoothConnect(invoke: Invoke) {
        try {
            if (!hasBtPermissions()) {
                invoke.reject("Permisos Bluetooth no concedidos. Abra Ajustes > Apps > Sistema Impresion > Permisos y active Bluetooth.")
                return
            }

            val mac = invoke.getArgs().optString("mac")?.takeIf { it.isNotEmpty() } ?: run {
                invoke.reject("Direccion MAC requerida")
                return
            }

            val adapter = android.bluetooth.BluetoothAdapter.getDefaultAdapter()
            if (adapter == null || !adapter.isEnabled) {
                invoke.reject("Bluetooth no disponible o desactivado")
                return
            }
            adapter.cancelDiscovery()

            val device = adapter.getRemoteDevice(mac)

            btSocket?.close()
            btSocket = device.createRfcommSocketToServiceRecord(SPP_UUID)
            btSocket?.connect()
            btOutputStream = btSocket?.outputStream

            val resp = JSObject()
            resp.put("connected", true)
            resp.put("mac", mac)
            invoke.resolve(resp)
        } catch (e: SecurityException) {
            invoke.reject("Permiso Bluetooth denegado. Conceda el permiso en Ajustes > Apps > Sistema Impresion.")
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
        val bytes = invoke.getArgs().optJSONArray("data")?.let { arr ->
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
        try {
            val resp = JSObject()
            resp.put("connected", btSocket?.isConnected == true)
            invoke.resolve(resp)
        } catch (e: Exception) {
            invoke.reject("Error consultando estado Bluetooth: ${e.message}")
        }
    }
}
