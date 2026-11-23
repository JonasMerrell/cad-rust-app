package org.rcad.mobile

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Colours and small builders for the screens, in the look of the OrcaSlicer fork's G-code page
 * (dark grey, translucent rounded panels with a thin border, Orca green), light when the phone is.
 */
object Ui {
    var dark = true
        private set
    var BG = 0xFF1D1F22.toInt()
    var PANEL = 0xEB24262A.toInt()
    var PANEL_SOLID = 0xFF24262A.toInt()
    var BORDER = 0xFF3A3D42.toInt()
    var TEXT = 0xFFE8E8E8.toInt()
    var MUTED = 0xFFA0A4AA.toInt()
    var TRACK = 0xFF4A4E55.toInt()
    var USER_BG = 0x2600AE42
    const val ACCENT = 0xFF00AE42.toInt()
    var ERR = 0xFFF85149.toInt()
    var WARN = 0xFFF0A030.toInt()

    /** Pick the palette for the phone's light or dark setting and colour the window. */
    fun init(a: Activity) {
        dark = (a.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) != Configuration.UI_MODE_NIGHT_NO
        if (dark) {
            BG = 0xFF1D1F22.toInt(); PANEL = 0xEB24262A.toInt(); PANEL_SOLID = 0xFF24262A.toInt(); BORDER = 0xFF3A3D42.toInt()
            TEXT = 0xFFE8E8E8.toInt(); MUTED = 0xFFA0A4AA.toInt(); TRACK = 0xFF4A4E55.toInt(); USER_BG = 0x2600AE42
            ERR = 0xFFF85149.toInt(); WARN = 0xFFF0A030.toInt()
        } else {
            BG = 0xFFE9EAEC.toInt(); PANEL = 0xF0FFFFFF.toInt(); PANEL_SOLID = 0xFFFFFFFF.toInt(); BORDER = 0xFFD0D3D8.toInt()
            TEXT = 0xFF222222.toInt(); MUTED = 0xFF5D6168.toInt(); TRACK = 0xFFC7CAD0.toInt(); USER_BG = 0x2200AE42
            ERR = 0xFFCF222E.toInt(); WARN = 0xFFB4540C.toInt()
        }
        a.window.statusBarColor = BG
        a.window.navigationBarColor = BG
        @Suppress("DEPRECATION")
        a.window.decorView.systemUiVisibility = if (dark) 0 else (View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR)
    }

    fun dialog(c: Context): AlertDialog.Builder =
        AlertDialog.Builder(c, if (dark) android.R.style.Theme_Material_Dialog_Alert else android.R.style.Theme_Material_Light_Dialog_Alert)

    fun dp(c: Context, v: Int): Int = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), c.resources.displayMetrics).toInt()

    fun rounded(color: Int, radius: Float, stroke: Int? = null, strokePx: Int = 2) = GradientDrawable().apply {
        setColor(color)
        cornerRadius = radius
        if (stroke != null) setStroke(strokePx, stroke)
    }

    /** An Orca-style floating panel: translucent, rounded, thin border. */
    fun panel(c: Context) = rounded(PANEL, dp(c, 8).toFloat(), BORDER, maxOf(1, dp(c, 1)))

    fun text(c: Context, s: CharSequence, size: Float = 14f, color: Int = TEXT, bold: Boolean = false) = TextView(c).apply {
        text = s
        textSize = size
        setTextColor(color)
        if (bold) typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        setTextIsSelectable(false)
    }

    /** Orca's buttons: outlined; `primary` is filled green. */
    fun button(c: Context, label: String, primary: Boolean = false, onClick: (View) -> Unit) = Button(c).apply {
        text = label
        isAllCaps = false
        textSize = 13.5f
        stateListAnimator = null
        setTextColor(if (primary) Color.WHITE else TEXT)
        background = if (primary) rounded(ACCENT, dp(c, 5).toFloat()) else rounded(Color.TRANSPARENT, dp(c, 5).toFloat(), BORDER, maxOf(1, dp(c, 1)))
        minHeight = dp(c, 34)
        minimumHeight = dp(c, 34)
        minWidth = dp(c, 34)
        minimumWidth = dp(c, 34)
        setPadding(dp(c, 11), 0, dp(c, 11), 0)
        setOnClickListener(onClick)
    }

    /** A tab of a segmented row: green text and underline when on. */
    fun tab(c: Context, label: String, onClick: (View) -> Unit) = TextView(c).apply {
        text = label
        textSize = 14f
        gravity = Gravity.CENTER
        setPadding(dp(c, 12), dp(c, 6), dp(c, 12), dp(c, 6))
        setOnClickListener(onClick)
    }

    fun setTab(t: TextView, on: Boolean) {
        t.setTextColor(if (on) ACCENT else MUTED)
        t.typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        t.background = if (on) underline(t.context) else null
    }

    private fun underline(c: Context): android.graphics.drawable.Drawable {
        val line = GradientDrawable().apply { setColor(ACCENT) }
        return android.graphics.drawable.LayerDrawable(arrayOf<android.graphics.drawable.Drawable>(line)).apply {
            setLayerGravity(0, Gravity.BOTTOM or Gravity.FILL_HORIZONTAL)
            setLayerHeight(0, maxOf(2, dp(c, 2)))
        }
    }

    fun row(c: Context, vararg views: View) = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        views.forEach { addView(it) }
    }

    fun column(c: Context, padH: Int = 20, vararg views: View) = LinearLayout(c).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(c, padH), dp(c, 8), dp(c, padH), 0)
        views.forEach { addView(it) }
    }

    /** A section title inside a panel (Orca's ".title"). */
    fun title(c: Context, s: String) = text(c, s, 13.5f, TEXT, bold = true).apply { setPadding(0, dp(c, 10), 0, dp(c, 4)) }

    /** A thin rule between sections. */
    fun rule(c: Context) = View(c).apply {
        setBackgroundColor(BORDER)
        layoutParams = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, maxOf(1, dp(c, 1))).apply { topMargin = dp(c, 8) }
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

    /** A one-character mark for a feature kind in the tree. */
    fun featureIcon(kind: String): String = when (kind) {
        "sketch" -> "✎"
        "extrude" -> "⬆"
        "revolve" -> "↻"
        "sweep", "loft" -> "∿"
        "fillet" -> "◠"
        "chamfer" -> "◺"
        "hole" -> "◎"
        "shell" -> "▢"
        "draft" -> "⟋"
        "mirror" -> "⇋"
        "plane" -> "▱"
        "helix" -> "⌇"
        "import" -> "⇩"
        "boolean" -> "⊕"
        "split" -> "⫽"
        "transform" -> "✥"
        "sheetMetal" -> "⌐"
        "custom" -> "ƒ"
        else -> if (kind.contains("pattern", ignoreCase = true)) "⋮" else "•"
    }
}
