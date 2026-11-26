package org.rcad.mobile

import android.content.Context
import android.os.SystemClock
import android.webkit.WebResourceResponse
import org.json.JSONArray
import org.json.JSONObject

/** When each step of showing a page in the viewer happened, from the moment the app asked for it. */
class ViewTiming {
    private var t0 = 0L
    private var what = ""
    private val marks = mutableListOf<Pair<String, Long>>()
    /** Bumped per load, so late callbacks of an earlier page are ignored. */
    var gen = 0
        private set
    var loading = false
    /** netdash showed its progress page: it's building the page, not the phone loading it */
    var building = false
    /** The browser's Navigation and Resource Timing, as JSON from [JS]. */
    var page: String? = null

    fun start(sel: String) {
        gen++
        t0 = SystemClock.elapsedRealtime()
        what = sel
        marks.clear()
        page = null
        loading = true
        building = false
    }

    fun mark(step: String) {
        if (loading && marks.none { it.first == step }) marks += step to SystemClock.elapsedRealtime() - t0
    }

    fun report(): String {
        if (t0 == 0L) return "Nothing loaded yet."
        val lines = mutableListOf("$what")
        val names = mapOf("html" to "page arrived from netdash", "loaded" to "page and scripts loaded", "first frame" to "first 3D frame drawn",
            "no first frame" to "gave up waiting for a first frame",
            "netdash building" to "netdash started building it (progress page)")
        marks.forEach { (k, v) -> lines += "${secs(v)}  ${names[k] ?: k}" }
        try {
            // evaluateJavascript hands back the returned string JSON-quoted
            val p = JSONObject(JSONArray("[${page ?: "\"{}\""}]").getString(0))
            p.optJSONObject("nav")?.let { n ->
                lines += ""
                lines += "In the page (from the request):"
                lines += "${secs(n.optLong("response"))}  netdash finished sending (${kb(n.optLong("size"))})"
                lines += "${secs(n.optLong("dom"))}  page parsed and its scripts run"
            }
            val markNames = mapOf("rcad-decoded" to "model data decoded", "rcad-built" to "3D scene built", "rcad-ready" to "page set up, first frame next")
            p.optJSONArray("marks")?.let { r ->
                for (i in 0 until r.length()) {
                    val e = r.getJSONArray(i)
                    lines += "${secs(e.optLong(1))}  ${markNames[e.optString(0)] ?: e.optString(0)}"
                }
            }
            p.optJSONArray("res")?.let { r ->
                for (i in 0 until r.length()) {
                    val e = r.getJSONArray(i)
                    val size = e.optLong(3)
                    lines += "${e.optString(0)}: ${secs(e.optLong(2))}${if (size > 0) ", ${kb(size)} over the network" else ", from the app or the cache"}"
                }
            }
        } catch (e: Exception) {
        }
        return lines.joinToString("\n")
    }

    companion object {
        private fun secs(ms: Long) = String.format("%.2f s", ms / 1000.0)
        private fun kb(b: Long) = if (b >= 1 shl 20) String.format("%.1f MB", b / 1048576.0) else "${b / 1024} KB"

        /** Run in the page: navigation and resource timing, in ms from the request's start. */
        const val JS = """(function(){var n=performance.getEntriesByType('navigation')[0]||{};
return JSON.stringify({nav:{response:Math.round(n.responseEnd||0),dom:Math.round(n.domContentLoadedEventEnd||0),size:n.transferSize||n.encodedBodySize||0},
marks:performance.getEntriesByType('mark').map(function(e){return [e.name,Math.round(e.startTime)];}),
res:performance.getEntriesByType('resource').map(function(e){return [e.name.split('?')[0].split('/').pop(),Math.round(e.startTime),Math.round(e.duration),e.transferSize||0];})});})()"""
    }
}

/**
 * Files the viewer pages load from the internet, shipped in the app instead: three.js 0.147.0 and
 * its OrbitControls (MIT, assets/three/LICENSE.txt), the same bytes cdn.jsdelivr.net serves. So the
 * viewer works without the internet and doesn't wait for a CDN.
 */
object Bundled {
    private const val CDN = "https://cdn.jsdelivr.net/npm/three@0.147.0/"
    private val files = mapOf(
        CDN + "build/three.min.js" to "three/three.min.js",
        CDN + "examples/js/controls/OrbitControls.js" to "three/OrbitControls.js",
    )

    fun response(c: Context, url: String?): WebResourceResponse? {
        val asset = files[url ?: return null] ?: return null
        return WebResourceResponse("application/javascript", "utf-8", c.assets.open(asset)).apply {
            responseHeaders = mapOf("Access-Control-Allow-Origin" to "*", "Cache-Control" to "max-age=31536000")
        }
    }
}
