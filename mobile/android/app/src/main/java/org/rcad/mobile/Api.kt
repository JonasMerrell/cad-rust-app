package org.rcad.mobile

import android.content.Context
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

class ApiException(val code: Int, msg: String) : IOException(msg)

/** Settings, kept in SharedPreferences. */
class Prefs(context: Context) {
    private val sp = context.applicationContext.getSharedPreferences("settings", Context.MODE_PRIVATE)

    /** netdash addresses, tried in order (home LAN first, then e.g. a VPN address). */
    var urls: List<String>
        get() = (sp.getString("urls", "") ?: "").split(',', '\n', ' ').map { it.trim().trimEnd('/') }.filter { it.isNotEmpty() }
        set(v) = sp.edit().putString("urls", v.joinToString(",")).apply()

    var token: String
        get() = sp.getString("token", "") ?: ""
        set(v) = sp.edit().putString("token", v.trim()).apply()

    var lastGoodUrl: String
        get() = sp.getString("lastGoodUrl", "") ?: ""
        set(v) = sp.edit().putString("lastGoodUrl", v).apply()

    /** opus, sonnet or haiku; empty = what netdash's rCAD is set to. */
    var model: String
        get() = sp.getString("model", "") ?: ""
        set(v) = sp.edit().putString("model", v).apply()

    /** The tray tab last open on a document: 0 features, 1 Claude. */
    var lastPane: Int
        get() = sp.getInt("lastPane", 0)
        set(v) = sp.edit().putInt("lastPane", v).apply()

    /** The chat last open for each document, to come back to it. */
    fun chatFor(doc: String): String? = sp.getString("chat:$doc", null)
    fun setChatFor(doc: String, chat: String?) = sp.edit().putString("chat:$doc", chat).apply()
    /** The tab last shown for a document, as "kind:index:page". */
    fun tabFor(doc: String): String? = sp.getString("tab:$doc", null)
    fun setTabFor(doc: String, tab: String) = sp.edit().putString("tab:$doc", tab).apply()

    fun orderedUrls(): List<String> {
        val all = urls
        val last = lastGoodUrl
        return if (last in all) listOf(last) + (all - last) else all
    }

    val paired: Boolean get() = urls.isNotEmpty() && token.isNotEmpty()
}

/** Client for the netdash service (capture/receiver: rcad_capture_receiver.py + rcad_projects.py). */
class Api(val base: String, val token: String) {

    private fun open(path: String, method: String, connectMs: Int = 4000, readMs: Int = 30000): HttpURLConnection {
        val c = URL(base + path).openConnection() as HttpURLConnection
        c.requestMethod = method
        c.connectTimeout = connectMs
        c.readTimeout = readMs
        c.setRequestProperty("Authorization", "Bearer $token")
        c.useCaches = false
        return c
    }

    private fun read(c: HttpURLConnection): JSONObject {
        try {
            val code = c.responseCode
            val stream = if (code in 200..299) c.inputStream else c.errorStream
            val text = stream?.bufferedReader()?.use { it.readText() } ?: ""
            val obj = try { JSONObject(text.ifEmpty { "{}" }) } catch (e: Exception) { JSONObject().put("error", text.take(200)) }
            if (code !in 200..299) throw ApiException(code, obj.optString("error", "HTTP $code"))
            return obj
        } finally {
            c.disconnect()
        }
    }

    fun get(path: String, readMs: Int = 30000): JSONObject = read(open(path, "GET", readMs = readMs))

    fun post(path: String, body: JSONObject, readMs: Int = 30000): JSONObject {
        val c = open(path, "POST", readMs = readMs)
        val bytes = body.toString().toByteArray()
        c.doOutput = true
        c.setRequestProperty("Content-Type", "application/json")
        c.setFixedLengthStreamingMode(bytes.size)
        c.outputStream.use { it.write(bytes) }
        return read(c)
    }

    fun ping(): JSONObject = read(open("/api/ping", "GET", connectMs = 2500, readMs = 4000))

    /** The viewer page's address (the web view sends no headers on its own, so the token goes in the query). */
    /** The page showing one tab; [sel] is the tab as a query ("studio=0", "cam=1&page=sheet"). */
    fun viewUrl(doc: String, sel: String): String = "$base/api/doc/${enc(doc)}/view?t=${enc(token)}&$sel"

    companion object {
        fun enc(s: String): String = URLEncoder.encode(s, "UTF-8").replace("+", "%20")

        /** The first netdash address that answers. Call off the main thread. */
        fun connect(prefs: Prefs): Api {
            if (!prefs.paired) throw IOException("Not paired with netdash yet")
            var last: IOException? = null
            for (url in prefs.orderedUrls()) {
                try {
                    val api = Api(url, prefs.token)
                    val ping = api.ping()
                    if (!ping.optBoolean("projects", false)) {
                        throw IOException("netdash's rCAD service is too old: update rcad_capture_receiver.py and rcad_projects.py")
                    }
                    prefs.lastGoodUrl = url
                    return api
                } catch (e: ApiException) {
                    last = if (e.code == 401) IOException("netdash refused the token: pair again") else e
                } catch (e: IOException) {
                    last = if (e.message?.startsWith("netdash") == true) e else IOException("Can't reach ${url.removePrefix("http://")}")
                }
            }
            throw last ?: IOException("No netdash address set")
        }
    }
}
