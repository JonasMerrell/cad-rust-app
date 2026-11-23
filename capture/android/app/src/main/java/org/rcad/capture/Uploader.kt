package org.rcad.capture

import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.WorkManager
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** Moves photos from the phone to netdash, deleting each once netdash confirms it. */
object Uploader {
    data class Status(val connected: Boolean, val host: String, val pending: Int, val error: String?)

    @Volatile var status = Status(false, "", 0, null)
        private set
    var listener: ((Status) -> Unit)? = null

    private val executor = Executors.newSingleThreadExecutor()
    private val running = AtomicBoolean(false)
    @Volatile private var dirty = false
    private val main = Handler(Looper.getMainLooper())

    /** Upload now (in this process) if possible; WorkManager retries later if not. */
    fun kick(context: Context) {
        val app = context.applicationContext
        dirty = true
        executor.execute { if (!drain(app)) scheduleRetry(app) }
    }

    fun scheduleRetry(context: Context) {
        val req = OneTimeWorkRequest.Builder(UploadWorker::class.java)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 15, TimeUnit.SECONDS)
            .setInitialDelay(10, TimeUnit.SECONDS)
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork("upload", ExistingWorkPolicy.KEEP, req)
    }

    private fun publish(context: Context, connected: Boolean, host: String, error: String?) {
        status = Status(connected, host, Store.pendingCount(context), error)
        val s = status
        main.post { listener?.invoke(s) }
    }

    /** Returns true when nothing is left to upload (or nothing could be done about it). */
    fun drain(context: Context): Boolean {
        if (!running.compareAndSet(false, true)) {
            dirty = true
            return true // the running pass picks it up
        }
        try {
            var ok: Boolean
            do {
                dirty = false
                ok = pass(context)
            } while (ok && dirty)
            return ok
        } finally {
            running.set(false)
        }
    }

    private fun connect(context: Context, prefs: Prefs): Pair<Api, String>? {
        var lastErr: String? = null
        for (url in prefs.orderedUrls()) {
            try {
                val api = Api(url, prefs.token)
                api.ping()
                prefs.lastGoodUrl = url
                return api to url
            } catch (e: ApiException) {
                lastErr = if (e.code == 401) "netdash refused the token: pair again" else e.message
            } catch (e: IOException) {
                lastErr = "can't reach ${url.removePrefix("http://")}"
            }
        }
        publish(context, false, "", lastErr ?: "not paired")
        return null
    }

    private fun pass(context: Context): Boolean {
        val prefs = Prefs(context)
        if (!prefs.paired) {
            publish(context, false, "", "not paired")
            return true
        }
        val sessions = Store.all(context)
        val work = sessions.filter { it.pendingPhotos().isNotEmpty() || (it.finished && it.writingCount() == 0) }
        if (work.isEmpty()) {
            // Just refresh the connection light.
            connect(context, prefs)?.let { publish(context, true, host(it.second), null) }
            return true
        }
        val (api, url) = connect(context, prefs) ?: return false
        publish(context, true, host(url), null)
        try {
            for (s in work) {
                if (s.serverId == null) {
                    val body = JSONObject()
                        .put("name", s.name)
                        .put("stamp", s.meta.optString("stamp"))
                        .put("created", s.meta.optString("created"))
                        .put("client_id", s.meta.optString("client_id"))
                        .put("notes", s.meta.optString("notes"))
                        .put("ruler_mm", s.meta.opt("ruler_mm"))
                        .put("device", "${Build.MANUFACTURER} ${Build.MODEL}")
                        .put("app", "rCAD Capture ${BuildConfigShim.VERSION}")
                        .put("camera", s.meta.optJSONObject("camera") ?: JSONObject())
                        .put("reference", s.reference ?: JSONObject.NULL)
                    s.serverId = api.postJson("/api/sessions", body).getString("id")
                }
                val sid = s.serverId!!
                for (f in s.pendingPhotos()) {
                    api.putPhoto(sid, f)
                    f.delete()
                    s.markUploaded()
                    publish(context, true, host(url), null)
                }
                if (s.finished && s.writingCount() == 0 && s.pendingPhotos().isEmpty()) {
                    api.postJson("/api/sessions/${Api.enc(sid)}/finish", JSONObject()
                        .put("captured", s.shots)
                        .put("reference", s.reference ?: JSONObject.NULL)
                        .put("ruler_mm", s.meta.opt("ruler_mm"))
                        .put("camera", s.meta.optJSONObject("camera") ?: JSONObject()))
                    s.dir.deleteRecursively()
                }
            }
            publish(context, true, host(url), null)
            return true
        } catch (e: ApiException) {
            publish(context, false, host(url), "netdash: ${e.message}")
            return false
        } catch (e: IOException) {
            publish(context, false, host(url), "upload interrupted: ${e.message}")
            return false
        }
    }

    private fun host(url: String) = url.removePrefix("http://").removePrefix("https://")
}

object BuildConfigShim {
    const val VERSION = "1.1"
}
