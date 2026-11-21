package org.rcad.capture

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.UUID

/**
 * Capture sessions waiting on the phone. Photos live only in the app's private
 * storage (never the gallery) and are deleted as soon as netdash confirms them.
 *
 *   files/sessions/<localId>/meta.json
 *   files/sessions/<localId>/IMG_0001.jpg   (a finished capture, waiting to upload)
 *   files/sessions/<localId>/.IMG_0002.jpg.part (still being written by the camera)
 */
class LocalSession(val dir: File) {
    val metaFile = File(dir, "meta.json")
    var meta: JSONObject = if (metaFile.exists()) JSONObject(metaFile.readText()) else JSONObject()

    val localId: String get() = dir.name
    val name: String get() = meta.optString("name", "scan")
    var serverId: String?
        get() = meta.optString("serverId", "").ifEmpty { null }
        set(v) { meta.put("serverId", v ?: ""); save() }
    var finished: Boolean
        get() = meta.optBoolean("finished", false)
        set(v) { meta.put("finished", v); save() }
    /** Photos taken in this session (also the next file number). */
    val shots: Int get() = meta.optInt("shots", 0)
    val uploaded: Int get() = meta.optInt("uploaded", 0)

    /** The known-size reference object for calibration photos, or null. */
    var reference: JSONObject?
        get() = meta.optJSONObject("reference")
        set(v) { meta.put("reference", v ?: JSONObject.NULL); save() }

    val calibrationShots: Int get() = meta.optInt("cal_shots", 0)

    /** Photos share one numbering; calibration photos (reference in view) are CAL_nnnn.jpg. */
    @Synchronized
    fun nextFile(calibration: Boolean): Pair<File, File> {
        val n = shots + 1
        meta.put("shots", n)
        if (calibration) meta.put("cal_shots", calibrationShots + 1)
        save()
        val name = String.format(Locale.US, if (calibration) "CAL_%04d.jpg" else "IMG_%04d.jpg", n)
        return File(dir, ".$name.part") to File(dir, name)
    }

    @Synchronized
    fun markUploaded() {
        meta.put("uploaded", uploaded + 1)
        save()
    }

    @Synchronized
    fun pendingPhotos(): List<File> =
        (dir.listFiles() ?: emptyArray()).filter { it.isFile && (it.name.startsWith("IMG_") || it.name.startsWith("CAL_")) && it.name.endsWith(".jpg") }.sortedBy { it.name }

    fun writingCount(): Int = (dir.listFiles() ?: emptyArray()).count { it.name.endsWith(".part") }

    @Synchronized
    fun save() {
        val tmp = File(dir, "meta.json.tmp")
        tmp.writeText(meta.toString(2))
        tmp.renameTo(metaFile)
    }
}

object Store {
    fun root(context: Context): File = File(context.noBackupFilesDir, "sessions").apply { mkdirs() }

    fun all(context: Context): List<LocalSession> =
        (root(context).listFiles() ?: emptyArray()).filter { it.isDirectory && File(it, "meta.json").exists() }
            .sortedBy { it.name }.map { LocalSession(it) }

    fun create(context: Context, name: String, notes: String, rulerMm: Double?): LocalSession {
        val now = Date()
        val stamp = SimpleDateFormat("yyyy-MM-dd_HHmm", Locale.US).format(now)
        val iso = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ssZ", Locale.US).format(now)
        val id = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(now) + "-" + UUID.randomUUID().toString().take(6)
        val dir = File(root(context), id).apply { mkdirs() }
        val s = LocalSession(dir)
        s.meta.put("name", name.ifBlank { "scan" })
        s.meta.put("notes", notes)
        s.meta.put("ruler_mm", rulerMm ?: JSONObject.NULL)
        s.meta.put("stamp", stamp)
        s.meta.put("created", iso)
        s.meta.put("client_id", id)
        s.meta.put("shots", 0)
        s.save()
        return s
    }

    fun pendingCount(context: Context): Int = all(context).sumOf { it.pendingPhotos().size }

    fun pendingBytes(context: Context): Long = all(context).sumOf { s -> s.pendingPhotos().sumOf { it.length() } }

    /** Leftover .part files from a crash are useless: remove them. */
    fun cleanStale(context: Context) {
        val cutoff = System.currentTimeMillis() - 10 * 60 * 1000
        all(context).forEach { s ->
            s.dir.listFiles()?.filter { it.name.endsWith(".part") && it.lastModified() < cutoff }?.forEach { it.delete() }
        }
    }
}
