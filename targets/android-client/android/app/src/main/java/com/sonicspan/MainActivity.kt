package com.sonicspan

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.SeekBar
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Minimal receiver UI: point it at a desktop node running
 * `desktop-node transmit --target <pc-ip>` and it plays the stream.
 */
class MainActivity : AppCompatActivity() {

    private lateinit var hostInput: EditText
    private lateinit var portInput: EditText
    private lateinit var connectButton: Button
    private lateinit var statusText: TextView
    private lateinit var statsText: TextView

    private var connected = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        hostInput = findViewById(R.id.host)
        portInput = findViewById(R.id.port)
        connectButton = findViewById(R.id.connect)
        statusText = findViewById(R.id.status)
        statsText = findViewById(R.id.stats)

        connectButton.setOnClickListener {
            if (connected) {
                nativeStop()
            } else {
                val port = portInput.text.toString().toIntOrNull() ?: 9000
                nativeStart(hostInput.text.toString().trim(), port)
            }
            connected = !connected
            connectButton.text = if (connected) "Stop" else "Connect"
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

    private external fun nativeStart(host: String, port: Int)
    private external fun nativeStop()
    private external fun nativeSetVolume(volume: Float)

    companion object {
        init {
            System.loadLibrary("android_client")
        }
    }
}
