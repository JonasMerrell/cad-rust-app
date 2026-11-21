package org.rcad.capture

import org.json.JSONObject
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.security.MessageDigest

class ApiException(val code: Int, msg: String) : IOException(msg)

/** Tiny client for capture/receiver/rcad_capture_receiver.py. */
class Api(private val base: String, private val token: String) {

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

    fun ping(): JSONObject = read(open("/api/ping", "GET", connectMs = 2500, readMs = 4000))

    fun postJson(path: String, body: JSONObject): JSONObject {
        val c = open(path, "POST")
        val bytes = body.toString().toByteArray()
        c.doOutput = true
        c.setRequestProperty("Content-Type", "application/json")
        c.setFixedLengthStreamingMode(bytes.size)
        c.outputStream.use { it.write(bytes) }
        return read(c)
    }

    /** Uploads one photo; returns only when netdash has it on disk with a matching SHA-256. */
    fun putPhoto(sessionId: String, file: File) {
        val sha = sha256(file)
        val c = open("/api/sessions/${enc(sessionId)}/photos/${enc(file.name)}", "PUT", readMs = 60000)
        c.doOutput = true
        c.setRequestProperty("Content-Type", "image/jpeg")
        c.setRequestProperty("X-Content-SHA256", sha)
        c.setFixedLengthStreamingMode(file.length())
        c.outputStream.use { out -> FileInputStream(file).use { it.copyTo(out, 1 shl 16) } }
        val res = read(c)
        if (res.optString("sha256") != sha) throw IOException("netdash stored a different file")
    }

    companion object {
        fun enc(s: String): String = URLEncoder.encode(s, "UTF-8").replace("+", "%20")

        fun sha256(f: File): String {
            val md = MessageDigest.getInstance("SHA-256")
            FileInputStream(f).use { inp ->
                val buf = ByteArray(1 shl 16)
                while (true) {
                    val n = inp.read(buf)
                    if (n < 0) break
                    md.update(buf, 0, n)
                }
            }
            return md.digest().joinToString("") { "%02x".format(it) }
        }
    }
}
