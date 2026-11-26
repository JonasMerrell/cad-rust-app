package org.rcad.mobile

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Path
import android.net.Uri
import android.view.MotionEvent
import android.view.View
import java.io.ByteArrayOutputStream

/** A picture going with the next chat request: a photo, or the model with pencil strokes on it. */
class Attachment(val jpeg: ByteArray, val thumb: Bitmap, val drawing: Boolean, val parts: List<String> = emptyList())

/**
 * The pencil: strokes drawn over the 3D view while it is on (the view underneath doesn't turn
 * then). [strokes] are in this view's pixels, which are the web view's.
 */
class PencilView(c: Context) : View(c) {
    val strokes = mutableListOf<MutableList<Pair<Float, Float>>>()
    var onChange: () -> Unit = {}
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = PENCIL
        style = Paint.Style.STROKE
        strokeWidth = Ui.dp(c, 4).toFloat()
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }

    fun clear() {
        strokes.clear()
        invalidate()
        onChange()
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> strokes += mutableListOf(e.x to e.y)
            MotionEvent.ACTION_MOVE -> {
                val s = strokes.lastOrNull() ?: return true
                for (h in 0 until e.historySize) s += e.getHistoricalX(h) to e.getHistoricalY(h)
                s += e.x to e.y
            }
            MotionEvent.ACTION_UP -> onChange()
        }
        invalidate()
        return true
    }

    override fun onDraw(canvas: Canvas) {
        for (s in strokes) canvas.drawPath(path(s, 1f), paint)
    }

    /** The strokes over [shot] (the view's picture, [scale] of its pixels per view pixel), as a JPEG. */
    fun onto(shot: Bitmap, scale: Float): Bitmap {
        val out = shot.copy(Bitmap.Config.ARGB_8888, true)
        val cv = Canvas(out)
        val p = Paint(paint).apply { strokeWidth = paint.strokeWidth * scale }
        for (s in strokes) cv.drawPath(path(s, scale), p)
        return out
    }

    /** About [n] points along the strokes, for asking the page which parts they cross. */
    fun samples(n: Int = 60): List<Pair<Float, Float>> {
        val all = strokes.flatten()
        if (all.size <= n) return all
        val step = all.size.toFloat() / n
        return (0 until n).map { all[(it * step).toInt()] }
    }

    private fun path(s: List<Pair<Float, Float>>, k: Float) = Path().apply {
        s.forEachIndexed { i, (x, y) -> if (i == 0) moveTo(x * k, y * k) else lineTo(x * k, y * k) }
        if (s.size == 1) lineTo(s[0].first * k + 0.1f, s[0].second * k)
    }

    companion object {
        const val PENCIL = 0xFFE5484D.toInt()
    }
}

object Photos {
    /** Longest side of a photo sent to Claude: plenty to see a part, and small to send. */
    private const val MAX_SIDE = 1600

    /** A photo from the gallery or the camera, upright, scaled down, as a JPEG (null if unreadable). */
    fun load(c: Context, uri: Uri): ByteArray? = try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        c.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_SIDE) sample *= 2
        val bmp = c.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, BitmapFactory.Options().apply { inSampleSize = sample }) }
        val turn = c.contentResolver.openInputStream(uri)?.use {
            when (android.media.ExifInterface(it).getAttributeInt(android.media.ExifInterface.TAG_ORIENTATION, 1)) {
                android.media.ExifInterface.ORIENTATION_ROTATE_90 -> 90f
                android.media.ExifInterface.ORIENTATION_ROTATE_180 -> 180f
                android.media.ExifInterface.ORIENTATION_ROTATE_270 -> 270f
                else -> 0f
            }
        } ?: 0f
        bmp?.let { jpeg(fit(it, MAX_SIDE, turn)) }
    } catch (e: Exception) {
        null
    }

    fun fit(b: Bitmap, side: Int, turn: Float = 0f): Bitmap {
        val k = minOf(1f, side.toFloat() / maxOf(b.width, b.height))
        if (k >= 1f && turn == 0f) return b
        val m = Matrix().apply { postScale(k, k); postRotate(turn) }
        return Bitmap.createBitmap(b, 0, 0, b.width, b.height, m, true)
    }

    fun jpeg(b: Bitmap, quality: Int = 85): ByteArray = ByteArrayOutputStream().also { b.compress(Bitmap.CompressFormat.JPEG, quality, it) }.toByteArray()

    fun thumb(jpeg: ByteArray, side: Int): Bitmap? = BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size)?.let { fit(it, side) }
}
