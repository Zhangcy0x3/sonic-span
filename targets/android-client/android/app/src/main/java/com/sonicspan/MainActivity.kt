package com.sonicspan

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.SeekBar
import android.widget.Spinner
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.appcompat.app.AppCompatActivity

/**
 * Minimal UI: receive a `desktop-node transmit` stream, or transmit this
 * device's microphone to `desktop-node receive`.
 */
class MainActivity : AppCompatActivity() {

    private lateinit var hostInput: EditText
    private lateinit var portInput: EditText
    private lateinit var modeSpinner: Spinner
    private lateinit var connectButton: Button
    private lateinit var statusText: TextView
    private lateinit var statsText: TextView

    private var connected = false
    private var pendingConnect: Pair<String, Int>? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        hostInput = findViewById(R.id.host)
        portInput = findViewById(R.id.port)
        modeSpinner = findViewById(R.id.mode)
        connectButton = findViewById(R.id.connect)
        statusText = findViewById(R.id.status)
        statsText = findViewById(R.id.stats)

        connectButton.setOnClickListener {
            if (connected) {
                nativeStop()
                setConnected(false)
                return@setOnClickListener
            }

            val host = hostInput.text.toString().trim()
            val port = portInput.text.toString().toIntOrNull() ?: 9000
            if (modeSpinner.selectedItemPosition == 1) {
                if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
                    == PackageManager.PERMISSION_GRANTED
                ) {
                    nativeStartTransmit(host, port)
                    setConnected(true)
                } else {
                    pendingConnect = host to port
                    requestPermissions(
                        arrayOf(Manifest.permission.RECORD_AUDIO),
                        PERMISSION_REQUEST_CODE
                    )
                }
            } else {
                nativeStart(host, port)
                setConnected(true)
            }
        }

        val volume = findViewById<SeekBar>(R.id.volume)
        volume.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(seekBar: SeekBar?, progress: Int, fromUser: Boolean) {
                nativeSetVolume(progress / 100f)
            }

            override fun onStartTrackingTouch(seekBar: SeekBar?) = Unit
            override fun onStopTrackingTouch(seekBar: SeekBar?) = Unit
        })
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != PERMISSION_REQUEST_CODE) return
        if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) {
            val (host, port) = pendingConnect ?: return
            nativeStartTransmit(host, port)
            setConnected(true)
        } else {
            statusText.text = "Microphone permission required"
        }
        pendingConnect = null
    }

    /** Called from the Rust receiver thread; hopped to the UI thread. */
    fun onStatus(message: String) {
        runOnUiThread { statusText.text = message }
    }

    /** Called from the Rust receiver thread; hopped to the UI thread. */
    fun onStats(packets: Long, lost: Long, underruns: Long) {
        runOnUiThread {
            statsText.text = "$packets packets · $lost lost · $underruns underruns"
        }
    }

    /** Called from the Rust transmitter thread; hopped to the UI thread. */
    fun onTxStats(packets: Long, bytes: Long) {
        runOnUiThread {
            statsText.text = "$packets packets sent · ${bytes / 1024} KiB"
        }
    }

    private fun setConnected(value: Boolean) {
        connected = value
        connectButton.text = if (value) "Stop" else "Connect"
    }

    private external fun nativeStart(host: String, port: Int)
    private external fun nativeStartTransmit(host: String, port: Int)
    private external fun nativeStop()
    private external fun nativeSetVolume(volume: Float)

    companion object {
        private const val PERMISSION_REQUEST_CODE = 1001

        init {
            System.loadLibrary("android_client")
        }
    }
}
