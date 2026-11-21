package org.rcad.capture

import android.content.Context
import androidx.work.Worker
import androidx.work.WorkerParameters

/** Finishes uploads after the app is closed or the network comes back. */
class UploadWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val done = Uploader.drain(applicationContext)
        return if (done && Store.pendingCount(applicationContext) == 0) Result.success() else Result.retry()
    }
}
