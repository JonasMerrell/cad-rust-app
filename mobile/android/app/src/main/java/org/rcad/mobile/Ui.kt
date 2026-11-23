package org.rcad.mobile

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ColorFilter
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.content.res.ColorStateList
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.graphics.PathParser

/**
 * Small helpers for building the screens in code, in the look of the OrcaSlicer phone app:
 * a dark top bar with tabs, a light 3D scene, and a white tray (bottom sheet) with grey section rows.
 */
object Ui {
    // OrcaSlicer's light theme, with a blue accent
    const val ACCENT = 0xFF2F80ED.toInt()
    /** The accent on the dark top bar (lighter, for contrast on #2D2D30). */
    const val ACCENT_ON_BAR = 0xFF6AA8F7.toInt()
    const val ACCENT_SOFT = 0xFFE3EEFD.toInt()
    const val BAR = 0xFF2D2D30.toInt()
    const val BAR_TEXT = 0xFFFFFFFF.toInt()
    const val BAR_DIM = 0xFFB6B6B6.toInt()
    const val SHEET = 0xFFFFFFFF.toInt()
    const val SECTION = 0xFFF7F7F7.toInt()
    const val LINE = 0xFFDBDBDB.toInt()
    const val LINE_SOFT = 0xFFEEEEEE.toInt()
    const val TEXT = 0xFF323A3D.toInt()
    const val MUTED = 0xFF6B6B6B.toInt()
    const val FAINT = 0xFF9B9B9B.toInt()
    const val ORANGE = 0xFFFF8000.toInt()
    const val ERR = 0xFFD9534F.toInt()
    const val SCENE = 0xFFEEF1F4.toInt()
    const val USER_BUBBLE = ACCENT_SOFT

    fun dp(c: Context, v: Int): Int = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), c.resources.displayMetrics).toInt()

    fun rounded(color: Int, radius: Float, stroke: Int? = null, strokeW: Int = 2) = GradientDrawable().apply {
        setColor(color)
        cornerRadius = radius
        if (stroke != null) setStroke(strokeW, stroke)
    }

    /** A background that ripples when tapped. */
    fun pressable(base: Drawable?, dark: Boolean = false) =
        RippleDrawable(ColorStateList.valueOf(if (dark) 0x33FFFFFF else 0x1F000000), base, base ?: GradientDrawable().apply { setColor(Color.WHITE) })

    fun text(c: Context, s: CharSequence, size: Float = 15f, color: Int = TEXT, bold: Boolean = false) = TextView(c).apply {
        text = s
        textSize = size
        setTextColor(color)
        if (bold) typeface = Typeface.DEFAULT_BOLD
        setTextIsSelectable(false)
    }

    /** The filled accent button (OrcaSlicer's "Slice plate", in blue). */
    fun accentButton(c: Context, label: String, onClick: (View) -> Unit) = TextView(c).apply {
        text = label
        textSize = 14f
        typeface = Typeface.DEFAULT_BOLD
        setTextColor(Color.WHITE)
        gravity = Gravity.CENTER
        background = pressable(rounded(ACCENT, dp(c, 6).toFloat()), dark = true)
        minHeight = dp(c, 36)
        setPadding(dp(c, 14), 0, dp(c, 14), 0)
        setOnClickListener(onClick)
    }

    /** White outlined button, as in the Orca sidebar. */
    fun outlineButton(c: Context, label: String, onClick: (View) -> Unit) = TextView(c).apply {
        text = label
        textSize = 14f
        setTextColor(TEXT)
        gravity = Gravity.CENTER
        background = pressable(rounded(SHEET, dp(c, 6).toFloat(), LINE, dp(c, 1)))
        minHeight = dp(c, 38)
        setPadding(dp(c, 14), 0, dp(c, 14), 0)
        setOnClickListener(onClick)
    }

    /** A preset combo box: outlined field with a chevron on the right. */
    fun combo(c: Context, label: String, onClick: (View) -> Unit) = TextView(c).apply {
        text = label
        textSize = 14f
        setTextColor(TEXT)
        gravity = Gravity.CENTER_VERTICAL
        isSingleLine = true
        ellipsize = android.text.TextUtils.TruncateAt.END
        background = pressable(rounded(SHEET, dp(c, 4).toFloat(), LINE, dp(c, 1)))
        minHeight = dp(c, 40)
        setPadding(dp(c, 10), 0, dp(c, 8), 0)
        setCompoundDrawablesRelative(null, null, Icon(c, "chevronDown", MUTED, 18), null)
        compoundDrawablePadding = dp(c, 6)
        setOnClickListener(onClick)
    }

    /** An icon button for a bar: dark (top bar) or light (tray, over the scene). */
    fun iconButton(c: Context, icon: String, color: Int, size: Int = 40, desc: String = icon, onClick: (View) -> Unit) = ImageView(c).apply {
        setImageDrawable(Icon(c, icon, color, 22))
        scaleType = ImageView.ScaleType.CENTER
        contentDescription = desc
        background = RippleDrawable(ColorStateList.valueOf(if (color == BAR_TEXT || color == BAR_DIM) 0x33FFFFFF else 0x1F000000), null,
            GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(Color.WHITE) })
        layoutParams = LinearLayout.LayoutParams(dp(c, size), dp(c, size))
        setOnClickListener(onClick)
    }

    /** A tab in the dark top bar (Prepare / Preview / Device in Orca): the accent when selected. */
    fun barTab(c: Context, label: String, onClick: (View) -> Unit) = TextView(c).apply {
        text = label
        textSize = 14f
        gravity = Gravity.CENTER
        setTextColor(BAR_DIM)
        setPadding(dp(c, 10), 0, dp(c, 10), 0)
        background = RippleDrawable(ColorStateList.valueOf(0x33FFFFFF), null, null)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.MATCH_PARENT)
        setOnClickListener(onClick)
    }

    fun selectTab(t: TextView, on: Boolean) {
        t.setTextColor(if (on) ACCENT_ON_BAR else BAR_DIM)
        t.typeface = if (on) Typeface.DEFAULT_BOLD else Typeface.DEFAULT
    }

    /** The dark top bar. */
    fun topBar(c: Context, vararg views: View) = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setBackgroundColor(BAR)
        setPadding(dp(c, 6), 0, dp(c, 6), 0)
        views.forEach { addView(it) }
    }

    /** A grey section header in the tray: icon, bold title, something on the right. */
    fun sectionRow(c: Context, icon: String?, title: String, right: View? = null) = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setBackgroundColor(SECTION)
        setPadding(dp(c, 12), dp(c, 8), dp(c, 12), dp(c, 8))
        minimumHeight = dp(c, 44)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(c, 4) }
        if (icon != null) addView(ImageView(c).apply { setImageDrawable(Icon(c, icon, TEXT, 20)) },
            LinearLayout.LayoutParams(dp(c, 20), dp(c, 20)).apply { marginEnd = dp(c, 8) })
        addView(text(c, title, 15f, TEXT, bold = true), LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        if (right != null) addView(right)
    }

    fun divider(c: Context, color: Int = LINE_SOFT) = View(c).apply {
        setBackgroundColor(color)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1)
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

    /**
     * The activity's root: [content] padded clear of the status bar, navigation bar and keyboard
     * (Android 15 draws apps edge to edge), with the top bar's colour behind the status bar and
     * [bottom] behind the navigation bar.
     */
    fun screen(c: Context, content: View, bottom: Int = SHEET): View {
        val bg = object : Drawable() {
            var top = 0
            private val p = Paint()
            override fun draw(canvas: Canvas) {
                val b = bounds
                p.color = bottom
                canvas.drawRect(b, p)
                p.color = BAR
                canvas.drawRect(b.left.toFloat(), b.top.toFloat(), b.right.toFloat(), (b.top + top).toFloat(), p)
            }
            override fun setAlpha(alpha: Int) {}
            override fun setColorFilter(cf: ColorFilter?) {}
            @Deprecated("Deprecated in Java")
            override fun getOpacity() = PixelFormat.OPAQUE
        }
        return android.widget.FrameLayout(c).apply {
            background = bg
            addView(content)
            setOnApplyWindowInsetsListener { v, insets ->
                val w = androidx.core.view.WindowInsetsCompat.toWindowInsetsCompat(insets, v)
                val bars = w.getInsets(androidx.core.view.WindowInsetsCompat.Type.systemBars())
                val ime = w.getInsets(androidx.core.view.WindowInsetsCompat.Type.ime())
                bg.top = bars.top
                v.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
                v.invalidate()
                insets
            }
        }
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

/** One of the editor's stroked icons ([IconPaths]) as a drawable, [sizeDp] square. */
class Icon(c: Context, name: String, color: Int, private val sizeDp: Int = 22) : Drawable() {
    private val px = Ui.dp(c, sizeDp)
    private val src: Path = try {
        PathParser.createPathFromPathData(IconPaths.all[name] ?: IconPaths.all.getValue("more"))
    } catch (e: RuntimeException) {
        Path()
    }
    private val path = Path()
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        this.color = color
    }

    init {
        setBounds(0, 0, px, px)
    }

    override fun getIntrinsicWidth() = px
    override fun getIntrinsicHeight() = px

    override fun draw(canvas: Canvas) {
        val b = bounds
        val s = minOf(b.width(), b.height()) / 24f
        path.reset()
        src.transform(Matrix().apply { setScale(s, s); postTranslate(b.left + (b.width() - 24 * s) / 2, b.top + (b.height() - 24 * s) / 2) }, path)
        paint.strokeWidth = 1.7f * s
        canvas.drawPath(path, paint)
    }

    override fun setAlpha(alpha: Int) {
        paint.alpha = alpha
    }

    override fun setColorFilter(colorFilter: ColorFilter?) {
        paint.colorFilter = colorFilter
    }

    @Deprecated("Deprecated in Java")
    override fun getOpacity() = PixelFormat.TRANSLUCENT
}
