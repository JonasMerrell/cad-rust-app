package org.rcad.capture

import android.content.Context

/** App settings, kept in SharedPreferences. */
class Prefs(context: Context) {
    private val sp = context.applicationContext.getSharedPreferences("settings", Context.MODE_PRIVATE)

    /** Receiver base URLs, tried in order (e.g. home LAN first, VPN second). */
    var urls: List<String>
        get() = (sp.getString("urls", "") ?: "").split(',', '\n', ' ').map { it.trim().trimEnd('/') }.filter { it.isNotEmpty() }
        set(v) = sp.edit().putString("urls", v.joinToString(",")).apply()

    var token: String
        get() = sp.getString("token", "") ?: ""
        set(v) = sp.edit().putString("token", v.trim()).apply()

    /** The URL that answered last, tried first next time. */
    var lastGoodUrl: String
        get() = sp.getString("lastGoodUrl", "") ?: ""
        set(v) = sp.edit().putString("lastGoodUrl", v).apply()

    /** Capture mode: 0 tap/hold burst, 1 interval, 2 walk-around (movement). */
    var mode: Int
        get() = sp.getInt("mode", 0)
        set(v) = sp.edit().putInt("mode", v).apply()

    var intervalSec: Float
        get() = sp.getFloat("intervalSec", 1.0f)
        set(v) = sp.edit().putFloat("intervalSec", v).apply()

    /** Walk-around mode takes a photo every this many degrees of turn. */
    var stepDeg: Float
        get() = sp.getFloat("stepDeg", 8f)
        set(v) = sp.edit().putFloat("stepDeg", v).apply()

    /** Lock exposure, focus and white balance on the first photo of a session. */
    var autoLock: Boolean
        get() = sp.getBoolean("autoLock", true)
        set(v) = sp.edit().putBoolean("autoLock", v).apply()

    fun orderedUrls(): List<String> {
        val all = urls
        val last = lastGoodUrl
        return if (last in all) listOf(last) + (all - last) else all
    }

    val paired: Boolean get() = urls.isNotEmpty() && token.isNotEmpty()
}
