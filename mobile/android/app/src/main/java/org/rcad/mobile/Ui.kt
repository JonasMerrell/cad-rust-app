package org.rcad.mobile

import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.util.TypedValue
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/** Small helpers for building the screens in code. */
object Ui {
    const val BG = 0xFF15171A.toInt()
    const val PANEL = 0xFF1F2328.toInt()
    const val PANEL2 = 0xFF2D333B.toInt()
    const val TEXT = 0xFFE6E6E6.toInt()
    const val MUTED = 0xFF9AA0A6.toInt()
    const val ACCENT = 0xFF3D8BFF.toInt()
    const val OK = 0xFF3FB950.toInt()
    const val ERR = 0xFFF85149.toInt()

    fun dp(c: Context, v: Int): Int = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), c.resources.displayMetrics).toInt()

    fun rounded(color: Int, radius: Float, stroke: Int? = null) = GradientDrawable().apply {
        setColor(color)
        cornerRadius = radius
        if (stroke != null) setStroke(2, stroke)
    }

    fun text(c: Context, s: CharSequence, size: Float = 15f, color: Int = TEXT, bold: Boolean = false) = TextView(c).apply {
        text = s
        textSize = size
        setTextColor(color)
        if (bold) typeface = Typeface.DEFAULT_BOLD
        setTextIsSelectable(false)
    }

    fun button(c: Context, label: String, primary: Boolean = false, onClick: (View) -> Unit) = Button(c).apply {
        text = label
        isAllCaps = false
        textSize = 14f
        setTextColor(if (primary) Color.WHITE else TEXT)
        background = rounded(if (primary) ACCENT else PANEL2, dp(c, 10).toFloat())
        minHeight = dp(c, 40)
        minimumHeight = dp(c, 40)
        setPadding(dp(c, 14), 0, dp(c, 14), 0)
        setOnClickListener(onClick)
    }

    fun row(c: Context, vararg views: View) = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = android.view.Gravity.CENTER_VERTICAL
        views.forEach { addView(it) }
    }

    fun column(c: Context, padH: Int = 20, vararg views: View) = LinearLayout(c).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(c, padH), dp(c, 8), dp(c, padH), 0)
        views.forEach { addView(it) }
    }

    /** "3 min ago" style time from Unix seconds. */
    fun ago(secs: Long): String {
        if (secs <= 0) return ""
        val d = System.currentTimeMillis() / 1000 - secs
        return when {
            d < 60 -> "just now"
            d < 3600 -> "${d / 60} min ago"
            d < 86400 -> "${d / 3600} h ago"
            d < 86400 * 30 -> "${d / 86400} d ago"
            else -> java.text.SimpleDateFormat("d MMM yyyy", java.util.Locale.getDefault()).format(java.util.Date(secs * 1000))
        }
    }
}
