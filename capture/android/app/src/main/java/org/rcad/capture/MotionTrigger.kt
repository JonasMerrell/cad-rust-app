package org.rcad.capture

import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import kotlin.math.abs
import kotlin.math.acos
import kotlin.math.min
import kotlin.math.sqrt

/**
 * Walk-around trigger: fires when the phone has turned [stepDeg] degrees since
 * the last photo and is held steady (so the photo isn't blurred).
 */
class MotionTrigger(private val sm: SensorManager, private val onShot: () -> Unit, private val onHint: (String) -> Unit) :
    SensorEventListener {

    var stepDeg = 8f
    private var lastShotQ: FloatArray? = null
    private val q = FloatArray(4)
    private var haveQ = false
    private var gyroMag = 0f
    private var steadySince = 0L
    private var lastShotAt = 0L
    private var lastHintAt = 0L
    var busy: () -> Boolean = { false }

    fun start() {
        lastShotQ = null
        val rv = sm.getDefaultSensor(Sensor.TYPE_GAME_ROTATION_VECTOR) ?: sm.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR)
        rv?.let { sm.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME) }
        sm.getDefaultSensor(Sensor.TYPE_GYROSCOPE)?.let { sm.registerListener(this, it, SensorManager.SENSOR_DELAY_GAME) }
        if (rv == null) onHint("This phone has no rotation sensor: use Interval mode")
    }

    fun stop() = sm.unregisterListener(this)

    /** Call after any photo so the next one waits for another step of turn. */
    fun shotTaken() {
        if (haveQ) lastShotQ = q.copyOf()
        lastShotAt = System.currentTimeMillis()
    }

    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {}

    override fun onSensorChanged(e: SensorEvent) {
        val now = System.currentTimeMillis()
        when (e.sensor.type) {
            Sensor.TYPE_GYROSCOPE -> {
                gyroMag = sqrt(e.values[0] * e.values[0] + e.values[1] * e.values[1] + e.values[2] * e.values[2])
                if (gyroMag > STEADY_RAD_S) steadySince = 0L else if (steadySince == 0L) steadySince = now
                return
            }
            else -> {
                SensorManager.getQuaternionFromVector(q, e.values)
                haveQ = true
            }
        }
        val last = lastShotQ
        if (last == null) {
            // First photo of a run: take it once steady.
            if (steady(now) && !busy()) fire(now)
            return
        }
        val dot = min(1f, abs(q[0] * last[0] + q[1] * last[1] + q[2] * last[2] + q[3] * last[3]))
        val deg = Math.toDegrees(2.0 * acos(dot.toDouble())).toFloat()
        if (deg >= stepDeg) {
            if (steady(now) && !busy() && now - lastShotAt > 350) fire(now)
            else if (now - lastHintAt > 700) {
                lastHintAt = now
                onHint("Hold still…")
            }
        } else if (now - lastHintAt > 700) {
            lastHintAt = now
            onHint(String.format("Turn %.0f° more", stepDeg - deg))
        }
    }

    private fun steady(now: Long) = steadySince != 0L && now - steadySince >= STEADY_MS

    private fun fire(now: Long) {
        lastShotAt = now
        if (haveQ) lastShotQ = q.copyOf()
        onShot()
    }

    companion object {
        const val STEADY_RAD_S = 0.25f
        const val STEADY_MS = 150L
    }
}
