package org.rcad.mobile

import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.graphics.BitmapFactory
import android.graphics.Paint
import android.os.Bundle
import android.util.Base64
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.InputMethodManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import androidx.coordinatorlayout.widget.CoordinatorLayout
import androidx.core.widget.NestedScrollView
import com.google.android.material.bottomsheet.BottomSheetBehavior
import org.json.JSONObject
import java.io.IOException

/**
 * One document, laid out like the OrcaSlicer phone app: rCAD's own web viewer (the page
 * export_web writes, made on netdash) fills the screen, and a tray slides up over it with two
 * tabs from the top bar: the feature tree, and a chat with Claude, which runs on netdash through
 * Claude Code. Chats are the same ones the editor's chat panel saves next to the document, so a
 * conversation can go on in either place.
 */
class DocActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var docId: String
    private var api: Api? = null
    private var detail: JSONObject? = null
    /** The feature tree the viewer page was built from (/tree): errors, parts, an assembly's instances and mates. */
    private var built: JSONObject? = null

    private var assembly: Int? = null
    private var studio: Int = 0
    private var chatId: String? = null
    private var jobId: String? = null
    private var destroyed = false
    /** The tray's tab: 0 features, 1 Claude. */
    private var pane = 0

    private lateinit var root: CoordinatorLayout
    private lateinit var stage: FrameLayout
    private lateinit var web: WebView
    private lateinit var webNote: TextView
    private lateinit var progressCard: LinearLayout
    private lateinit var progressText: TextView
    private lateinit var sheet: LinearLayout
    private lateinit var sheetBehavior: BottomSheetBehavior<LinearLayout>
    private lateinit var tabFeatures: TextView
    private lateinit var tabClaude: TextView
    private lateinit var treePane: NestedScrollView
    private lateinit var tree: LinearLayout
    private lateinit var chatPane: LinearLayout
    private lateinit var msgs: LinearLayout
    private lateinit var msgScroll: NestedScrollView
    private lateinit var input: EditText
    private lateinit var send: TextView
    private lateinit var tabCombo: TextView
    private lateinit var chatBtn: TextView
    private lateinit var modelBtn: TextView

    // the streaming reply
    private var assistant: TextView? = null
    private var thinking: TextView? = null
    private var pendingTool: TextView? = null
    private var pendingToolBox: LinearLayout? = null
    private var attemptAt = 0
    private var attemptLen: Int? = null
    private var docChanged = false

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        docId = intent.getStringExtra("doc") ?: run { finish(); return }
        chatId = prefs.chatFor(docId)
        val name = intent.getStringExtra("name") ?: "Document"
        pane = savedInstanceState?.getInt("pane") ?: if (prefs.lastPane == 1) 1 else 0

        // ---- top bar: back · name · Features | Claude · ⋮
        val back = Ui.iconButton(this, "chevronRight", Ui.BAR_TEXT, desc = "Back") { finish() }.apply { rotation = 180f }
        val title = Ui.text(this, name, 16f, Ui.BAR_TEXT, bold = true).apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply { marginEnd = Ui.dp(context, 4) }
        }
        tabFeatures = Ui.barTab(this, "Features") { showPane(0, expand = true) }
        tabClaude = Ui.barTab(this, "Claude") { showPane(1, expand = true) }
        val more = Ui.iconButton(this, "more", Ui.BAR_TEXT, desc = "More") { v -> moreMenu(v) }
        val bar = Ui.topBar(this, back, title, tabFeatures, tabClaude, more)

        // ---- the 3D scene
        web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.loadWithOverviewMode = true
            settings.useWideViewPort = true
            setBackgroundColor(Ui.SCENE)
            webViewClient = object : WebViewClient() {
                override fun onPageFinished(view: WebView?, url: String?) {
                    if (url?.startsWith("http") != true) return
                    webNote.visibility = View.GONE
                    // the page is built and cached now, so the tree it was built from comes quickly
                    loadBuiltTree()
                }

                override fun onReceivedHttpError(view: WebView?, request: WebResourceRequest?, error: WebResourceResponse?) {
                    if (request?.isForMainFrame == true) {
                        val why = try {
                            JSONObject(error?.data?.bufferedReader()?.use { it.readText() } ?: "{}").optString("error")
                        } catch (e: Exception) { "" }
                        showWebNote("Couldn't show the model: ${why.ifEmpty { "HTTP ${error?.statusCode}" }}")
                    }
                }

                override fun shouldOverrideUrlLoading(view: WebView?, request: WebResourceRequest?): Boolean = false
            }
        }
        webNote = Ui.text(this, "Building the 3D view on netdash…", 14f, Ui.MUTED).apply {
            setPadding(Ui.dp(context, 24), Ui.dp(context, 24), Ui.dp(context, 24), Ui.dp(context, 24))
            setBackgroundColor(Ui.SCENE)
            gravity = Gravity.CENTER
        }
        progressText = Ui.text(this, "Claude is working…", 13f)
        progressCard = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = Ui.rounded(0xF2FFFFFF.toInt(), Ui.dp(context, 8).toFloat(), 0x33000000, Ui.dp(context, 1))
            setPadding(Ui.dp(context, 12), Ui.dp(context, 10), Ui.dp(context, 12), Ui.dp(context, 10))
            visibility = View.GONE
            addView(progressText)
            addView(ProgressBar(context, null, android.R.attr.progressBarStyleHorizontal).apply {
                isIndeterminate = true
                indeterminateTintList = android.content.res.ColorStateList.valueOf(Ui.ACCENT)
            }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(context, 6)).apply { topMargin = Ui.dp(context, 6) })
            setOnClickListener { showPane(1, expand = true) }
        }
        stage = FrameLayout(this).apply {
            setBackgroundColor(Ui.SCENE)
            addView(web, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(webNote, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(progressCard, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM).apply {
                val m = Ui.dp(context, 10)
                setMargins(m, m, m, m)
            })
        }

        // ---- the tray: feature tree
        tree = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, 0, 0, Ui.dp(context, 24))
        }
        treePane = NestedScrollView(this).apply {
            isFillViewport = true
            addView(tree)
        }

        // ---- the tray: Claude
        chatBtn = Ui.combo(this, "New chat") { pickChat() }
        modelBtn = Ui.combo(this, modelLabel()) { pickModel() }
        val chatHead = Ui.sectionRow(this, "chat", "Claude", Ui.text(this, "runs on netdash", 12.5f, Ui.MUTED)).apply {
            (layoutParams as LinearLayout.LayoutParams).topMargin = 0
        }
        val chatBar = Ui.row(this).apply {
            setPadding(Ui.dp(context, 12), Ui.dp(context, 8), Ui.dp(context, 12), Ui.dp(context, 8))
            addView(chatBtn, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(modelBtn, LinearLayout.LayoutParams(Ui.dp(context, 120), ViewGroup.LayoutParams.WRAP_CONTENT).apply { marginStart = Ui.dp(context, 8) })
        }
        msgs = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(Ui.dp(context, 12), Ui.dp(context, 4), Ui.dp(context, 12), Ui.dp(context, 12))
        }
        msgScroll = NestedScrollView(this).apply { addView(msgs) }
        input = EditText(this).apply {
            hint = "Ask Claude to design or change something"
            setTextColor(Ui.TEXT)
            setHintTextColor(Ui.FAINT)
            textSize = 15f
            maxLines = 5
            background = Ui.rounded(Ui.SHEET, Ui.dp(context, 4).toFloat(), Ui.LINE, Ui.dp(context, 1))
            setPadding(Ui.dp(context, 10), Ui.dp(context, 9), Ui.dp(context, 10), Ui.dp(context, 9))
            setOnFocusChangeListener { _, has -> if (has) sheetBehavior.state = BottomSheetBehavior.STATE_EXPANDED }
            setOnClickListener { sheetBehavior.state = BottomSheetBehavior.STATE_EXPANDED }
        }
        send = Ui.accentButton(this, "Send") { if (jobId != null) stopJob() else sendPrompt() }
        val inputRow = Ui.row(this).apply {
            setPadding(Ui.dp(context, 12), Ui.dp(context, 8), Ui.dp(context, 12), Ui.dp(context, 10))
            setBackgroundColor(Ui.SHEET)
            addView(input, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(send, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, Ui.dp(context, 42)).apply { marginStart = Ui.dp(context, 8) })
        }
        chatPane = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(chatHead)
            addView(chatBar)
            addView(Ui.divider(context))
            addView(msgScroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(Ui.divider(context))
            addView(inputRow)
        }

        sheet = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = android.graphics.drawable.GradientDrawable().apply {
                setColor(Ui.SHEET)
                val r = Ui.dp(context, 14).toFloat()
                cornerRadii = floatArrayOf(r, r, r, r, 0f, 0f, 0f, 0f)
            }
            elevation = Ui.dp(context, 12).toFloat()
            addView(View(context).apply { background = Ui.rounded(0xFFC9C9C9.toInt(), Ui.dp(context, 2).toFloat()) },
                LinearLayout.LayoutParams(Ui.dp(context, 36), Ui.dp(context, 4)).apply {
                    gravity = Gravity.CENTER_HORIZONTAL
                    topMargin = Ui.dp(context, 8)
                    bottomMargin = Ui.dp(context, 6)
                })
            addView(treePane, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(chatPane, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        }

        root = CoordinatorLayout(this).apply {
            setBackgroundColor(Ui.SCENE)
            addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                addView(bar, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(context, 52)))
                addView(stage, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            }, CoordinatorLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(sheet, CoordinatorLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT).apply {
                behavior = BottomSheetBehavior<LinearLayout>().apply {
                    isFitToContents = false
                    halfExpandedRatio = 0.5f
                    isHideable = false
                    peekHeight = Ui.dp(context, 120)
                    // leave the top bar showing when the tray is all the way up
                    expandedOffset = Ui.dp(context, 52)
                    state = BottomSheetBehavior.STATE_HALF_EXPANDED
                }
            })
        }
        @Suppress("UNCHECKED_CAST")
        sheetBehavior = BottomSheetBehavior.from(sheet)
        sheetBehavior.addBottomSheetCallback(object : BottomSheetBehavior.BottomSheetCallback() {
            override fun onStateChanged(v: View, newState: Int) = fitStage()
            override fun onSlide(v: View, offset: Float) = fitStage()
        })
        root.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> fitStage() }

        setContentView(Ui.screen(this, root))
        showPane(pane, expand = false)
        buildTree()
        load(first = true)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putInt("pane", pane)
    }

    override fun onDestroy() {
        destroyed = true
        if (::web.isInitialized) web.destroy()
        super.onDestroy()
    }

    override fun onBackPressed() {
        if (::sheetBehavior.isInitialized && sheetBehavior.state == BottomSheetBehavior.STATE_EXPANDED) {
            sheetBehavior.state = BottomSheetBehavior.STATE_HALF_EXPANDED
            return
        }
        @Suppress("DEPRECATION")
        super.onBackPressed()
    }

    /**
     * Keep the 3D view to the part of the screen above the tray (so the model stays framed and
     * the viewer's buttons stay reachable), and the tray's contents to its visible part (so the
     * chat's input row sits on screen at every height).
     */
    private var lastFit = -1
    private fun fitStage() {
        val h = root.height
        if (h == 0) return
        val top = sheet.top.coerceIn(0, h)
        if (top == lastFit) return
        lastFit = top
        // the stage starts under the 52 dp top bar; keep at least 80 dp of it so the WebView never collapses
        stage.setPadding(0, 0, 0, (h - top).coerceAtMost(h - Ui.dp(this, 52 + 80)).coerceAtLeast(0))
        // the sheet is as tall as the screen; pad off the part that is below the bottom edge
        sheet.setPadding(0, 0, 0, (sheet.height - (h - top)).coerceAtLeast(0))
    }

    private fun showPane(p: Int, expand: Boolean) {
        pane = p
        prefs.lastPane = p
        Ui.selectTab(tabFeatures, p == 0)
        Ui.selectTab(tabClaude, p == 1)
        treePane.visibility = if (p == 0) View.VISIBLE else View.GONE
        chatPane.visibility = if (p == 1) View.VISIBLE else View.GONE
        if (expand && sheetBehavior.state == BottomSheetBehavior.STATE_COLLAPSED) sheetBehavior.state = BottomSheetBehavior.STATE_HALF_EXPANDED
        if (p == 1) scrollDown()
    }

    private fun moreMenu(anchor: View) {
        val m = android.widget.PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Reload the 3D view")
        m.menu.add(0, 2, 1, "Claude model…")
        m.menu.add(0, 3, 2, "Chats about this document…")
        m.setOnMenuItemClickListener {
            when (it.itemId) {
                1 -> { load(); reloadViewer() }
                2 -> pickModel()
                3 -> { showPane(1, expand = true); pickChat() }
            }
            true
        }
        m.show()
    }

    private fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()

    private fun showWebNote(t: String) {
        webNote.text = t
        webNote.visibility = View.VISIBLE
    }

    // ---------------------------------------------------------------- feature tree

    private fun tabName() = assembly?.let { names("assemblies").getOrNull(it) } ?: names("studios").getOrNull(studio) ?: "Part Studio 1"

    /** The tray's Features tab: which tab is shown, the Part Studio's features with the rollback bar, and its variables. */
    private fun buildTree() {
        tree.removeAllViews()
        val studios = names("studios")
        val asms = names("assemblies")
        val showing = Ui.sectionRow(this, if (assembly != null) "assembly" else "partStudio", if (assembly != null) "Assembly" else "Part Studio",
            Ui.text(this, if (studios.size + asms.size > 1) "${studios.size + asms.size} tabs" else "", 12.5f, Ui.MUTED)).apply {
            (layoutParams as LinearLayout.LayoutParams).topMargin = 0
        }
        tree.addView(showing)
        tabCombo = Ui.combo(this, tabName()) { pickTab() }
        tree.addView(LinearLayout(this).apply {
            setPadding(Ui.dp(context, 12), Ui.dp(context, 8), Ui.dp(context, 12), Ui.dp(context, 10))
            addView(tabCombo, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        })

        val b = built
        if (assembly != null) {
            if (b != null && b.optString("kind") == "assembly") assemblyTree(b)
            else tree.addView(note("Loading the assembly…"))
            return
        }
        val t = detail?.optJSONArray("trees")?.optJSONObject(studio)
        // errors from the build, by feature id
        val errors = HashMap<Int, String>()
        if (b != null) for (f in objs(b.optJSONArray("features"))) if (f.has("error")) errors[f.optInt("id")] = f.optString("error")
        if (detail == null) {
            tree.addView(note("Loading the feature tree…"))
            return
        }
        val feats = t?.optJSONArray("features")
        val n = feats?.length() ?: 0
        val studioName = studios.getOrNull(studio) ?: "Part Studio"
        tree.addView(Ui.sectionRow(this, "features", "Features",
            Ui.text(this, if (errors.isNotEmpty()) "${errors.size} failing" else "$n", 12.5f, if (errors.isNotEmpty()) Ui.ERR else Ui.MUTED)))
        if (t == null) {
            tree.addView(note("This netdash service doesn't send the feature tree yet; update rcad_projects.py there."))
            return
        }
        if (n == 0) tree.addView(note("No features yet. Ask Claude to make something."))
        val rb = if (t.isNull("rollback")) n else t.optInt("rollback", n).coerceIn(0, n)
        for (i in 0..n) {
            if (i == rb && rb < n) tree.addView(rollbackBar())
            if (i == n) break
            val f = feats!!.getJSONObject(i)
            tree.addView(featureRow(f, rolledBack = i >= rb, error = errors[f.optInt("id")]))
        }

        if (b != null) partsSection(b)

        val vars = t.optJSONArray("variables")
        if (vars != null && vars.length() > 0) {
            tree.addView(Ui.sectionRow(this, "variable", "Variables", Ui.text(this, "${vars.length()}", 12.5f, Ui.MUTED)))
            for (i in 0 until vars.length()) {
                val v = vars.getJSONObject(i)
                val vn = v.optString("name")
                tree.addView(treeRow("variable", "$vn = ${v.optString("expr")}", "", dim = false, strike = false, hidden = false) {
                    askAbout("Set $vn to ")
                })
            }
        }
    }

    private fun loadBuiltTree() {
        val a = api ?: return
        val sel = if (assembly != null) "assembly=$assembly" else "studio=$studio"
        val want = assembly to studio
        Thread {
            val t = try {
                a.get("/api/doc/${Api.enc(docId)}/tree?$sel", readMs = 16 * 60 * 1000)
            } catch (e: IOException) {
                null
            }
            runOnUiThread {
                if (t != null && want == (assembly to studio)) {
                    built = t
                    buildTree()
                }
            }
        }.start()
    }

    private fun objs(a: org.json.JSONArray?): List<JSONObject> = (0 until (a?.length() ?: 0)).mapNotNull { a!!.optJSONObject(it) }

    /** An assembly's instances and mates, from the built tree. */
    private fun assemblyTree(t: JSONObject) {
        val inst = objs(t.optJSONArray("instances"))
        val mates = objs(t.optJSONArray("mates"))
        val failed = t.optJSONArray("failed_mates")?.length() ?: 0
        tree.addView(Ui.sectionRow(this, "part", "Instances", Ui.text(this, "${inst.size}", 12.5f, Ui.MUTED)))
        for (i in inst) {
            val sub = listOfNotNull("sub-assembly".takeIf { i.optBoolean("sub") }, "fixed".takeIf { i.optBoolean("fixed") }, "hidden".takeIf { i.optBoolean("hidden") }).joinToString(" · ")
            val n = i.optString("name")
            tree.addView(treeRow(if (i.optBoolean("sub")) "assembly" else "part", n, sub, dim = i.optBoolean("hidden"), strike = false, hidden = i.optBoolean("hidden")) {
                askAbout("In the instance \"$n\": ")
            })
        }
        if (mates.isNotEmpty() || failed > 0) {
            tree.addView(Ui.sectionRow(this, "mateFastened", "Mates",
                Ui.text(this, if (failed > 0) "$failed failed" else "${mates.size}", 12.5f, if (failed > 0) Ui.ERR else Ui.MUTED)))
            for (m in mates) {
                val n = m.optString("name")
                val kind = m.optString("kind")
                val icon = "mate" + kind.replaceFirstChar { it.uppercase() }
                tree.addView(treeRow(if (IconPaths.all.containsKey(icon)) icon else "mateFastened", n, kind, dim = false, strike = false, hidden = false) {
                    askAbout("In the mate \"$n\": ")
                })
            }
        }
    }

    /** A Part Studio's parts with their size, from the built tree. */
    private fun partsSection(t: JSONObject) {
        val parts = objs(t.optJSONArray("parts"))
        if (parts.isEmpty()) return
        tree.addView(Ui.sectionRow(this, "part", "Parts", Ui.text(this, "${parts.size}", 12.5f, Ui.MUTED)))
        for (p in parts) {
            val n = p.optString("name")
            val bb = p.optJSONArray("bbox_mm")
            val size = if (bb != null && bb.length() == 2) {
                val lo = bb.optJSONArray(0)
                val hi = bb.optJSONArray(1)
                (0..2).joinToString(" × ") { k -> fmt(hi.optDouble(k) - lo.optDouble(k)) } + " mm"
            } else ""
            val vol = p.optDouble("volume_mm3", Double.NaN)
            val v = if (vol.isNaN()) "" else if (vol >= 1000) "%.1f cm³".format(vol / 1000) else "%.0f mm³".format(vol)
            tree.addView(treeRow("part", n, listOf(size, v).filter { it.isNotEmpty() }.joinToString(" · "), dim = false, strike = false, hidden = false) {
                askAbout("In the part \"$n\": ")
            })
        }
    }

    private fun fmt(x: Double) = if (Math.abs(x - Math.rint(x)) < 0.005) "%.0f".format(x) else "%.1f".format(x)

    private fun note(t: String) = Ui.text(this, t, 13.5f, Ui.MUTED).apply {
        setPadding(Ui.dp(context, 14), Ui.dp(context, 12), Ui.dp(context, 14), Ui.dp(context, 12))
    }

    private fun featureRow(f: JSONObject, rolledBack: Boolean, error: String?): View {
        val name = f.optString("name")
        val type = f.optString("type")
        val suppressed = f.optBoolean("suppressed")
        val extra = error ?: listOfNotNull(
            f.optString("detail").takeIf { it.isNotEmpty() },
            "suppressed".takeIf { suppressed },
        ).joinToString(" · ")
        return treeRow(type.ifEmpty { "custom" }, name, extra, dim = rolledBack || suppressed, strike = suppressed, hidden = f.optBoolean("hidden"), error = error != null) {
            featureMenu(f, error)
        }
    }

    private fun treeRow(icon: String, name: String, sub: String, dim: Boolean, strike: Boolean, hidden: Boolean, error: Boolean = false, onTap: () -> Unit): View {
        val col = if (error) Ui.ERR else if (dim) Ui.FAINT else Ui.TEXT
        val texts = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(Ui.text(context, name, 14.5f, col).apply {
                isSingleLine = true
                ellipsize = android.text.TextUtils.TruncateAt.END
                if (strike) paintFlags = paintFlags or Paint.STRIKE_THRU_TEXT_FLAG
            })
            if (sub.isNotEmpty()) addView(Ui.text(context, sub, 12f, if (error) Ui.ERR else if (dim) Ui.FAINT else Ui.MUTED).apply {
                maxLines = if (error) 3 else 1
                ellipsize = android.text.TextUtils.TruncateAt.END
            })
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.pressable(null)
            minimumHeight = Ui.dp(context, 48)
            setPadding(Ui.dp(context, 14), Ui.dp(context, 6), Ui.dp(context, 12), Ui.dp(context, 6))
            addView(ImageView(context).apply { setImageDrawable(Icon(context, if (error) "warning" else icon, if (error) Ui.ERR else if (dim) Ui.FAINT else Ui.ACCENT, 20)) },
                LinearLayout.LayoutParams(Ui.dp(context, 20), Ui.dp(context, 20)).apply { marginEnd = Ui.dp(context, 12) })
            addView(texts, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            if (hidden) addView(ImageView(context).apply { setImageDrawable(Icon(context, "eyeOff", Ui.FAINT, 18)) })
            setOnClickListener { onTap() }
        }
    }

    /** The editor's rollback bar: features below it are not built. */
    private fun rollbackBar() = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(Ui.dp(context, 14), Ui.dp(context, 4), Ui.dp(context, 14), Ui.dp(context, 4))
        addView(ImageView(context).apply { setImageDrawable(Icon(context, "rollback", Ui.ACCENT, 18)) })
        addView(View(context).apply { setBackgroundColor(Ui.ACCENT) },
            LinearLayout.LayoutParams(0, Ui.dp(context, 3), 1f).apply { marginStart = Ui.dp(context, 8); marginEnd = Ui.dp(context, 8) })
        addView(Ui.text(context, "rolled back", 12f, Ui.ACCENT))
    }

    private fun featureMenu(f: JSONObject, error: String?) {
        val name = f.optString("name")
        val suppressed = f.optBoolean("suppressed")
        val items = arrayOf(if (error != null) "Ask Claude to fix it" else "Ask Claude about $name…", if (suppressed) "Unsuppress it" else "Suppress it", "Delete it")
        AlertDialog.Builder(this)
            .setTitle(if (error != null) "$name: $error" else name)
            .setItems(items) { _, i ->
                when (i) {
                    0 -> if (error != null) askAbout("\"$name\" fails: $error. Fix it.", send = true) else askAbout("In \"$name\": ")
                    1 -> askAbout(if (suppressed) "Unsuppress \"$name\"." else "Suppress \"$name\".", send = true)
                    2 -> AlertDialog.Builder(this).setTitle("Delete $name?")
                        .setMessage("Claude deletes it on netdash. Features that use it may fail.")
                        .setPositiveButton("Delete") { _, _ -> askAbout("Delete the feature \"$name\".", send = true) }
                        .setNegativeButton("Cancel", null).show()
                }
            }.show()
    }

    /** Switch to Claude with [text] in the box, ready to finish (or sent right away). */
    private fun askAbout(text: String, send: Boolean = false) {
        showPane(1, expand = true)
        sheetBehavior.state = BottomSheetBehavior.STATE_EXPANDED
        input.setText(text)
        input.setSelection(text.length)
        if (send && jobId == null) {
            sendPrompt()
            return
        }
        input.requestFocus()
        (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager).showSoftInput(input, 0)
    }

    // ---------------------------------------------------------------- document

    /** Fetch the document's tabs and chats; the first time also open the viewer and the last chat. */
    private fun load(first: Boolean = false) {
        Thread {
            try {
                val a = api ?: Api.connect(prefs).also { api = it }
                val d = a.get("/api/doc/${Api.enc(docId)}")
                runOnUiThread {
                    detail = d
                    buildTree()
                    if (first) {
                        reloadViewer()
                        val running = d.optString("job").takeIf { d.has("job") && !d.isNull("job") && it.isNotEmpty() }
                        val known = chatId?.let { id -> chats().any { it.optString("id") == id } } == true
                        if (!known) chatId = null
                        if (running != null) attach(running) else openChat(chatId)
                    }
                }
            } catch (e: IOException) {
                runOnUiThread {
                    showWebNote(e.message ?: "Can't reach netdash")
                    if (first) notice(e.message ?: "Can't reach netdash", true)
                }
            }
        }.start()
    }

    private fun chats(): List<JSONObject> {
        val arr = detail?.optJSONArray("chat_list") ?: return emptyList()
        return (0 until arr.length()).map { arr.getJSONObject(it) }
    }

    private fun names(key: String): List<String> {
        val arr = detail?.optJSONArray(key) ?: return emptyList()
        return (0 until arr.length()).map { arr.optString(it) }
    }

    private fun reloadViewer() {
        val a = api ?: return
        showWebNote("Building the 3D view on netdash… (a big model can take a minute)")
        built = null
        buildTree()
        // app mode: the page drops its own side panel for a "Model" button and takes the app's colours
        web.loadUrl(a.viewUrl(docId, assembly, studio) + "&app=1")
    }

    private fun pickTab() {
        val studios = names("studios")
        val asms = names("assemblies")
        val labels = studios.map { "Part Studio: $it" } + asms.map { "Assembly: $it" }
        if (labels.isEmpty()) return
        AlertDialog.Builder(this).setTitle("Show").setItems(labels.toTypedArray()) { _, i ->
            if (i < studios.size) {
                studio = i
                assembly = null
            } else {
                assembly = i - studios.size
            }
            buildTree()
            reloadViewer()
        }.show()
    }

    // ---------------------------------------------------------------- chats

    private fun modelLabel() = when (prefs.model) {
        "opus" -> "Opus"
        "sonnet" -> "Sonnet"
        "haiku" -> "Haiku"
        else -> "Model"
    }

    private fun pickModel() {
        val keys = listOf("", "opus", "sonnet", "haiku")
        val labels = arrayOf("As set in rCAD on netdash", "Opus (plans, hands work to Sonnet/Haiku)", "Sonnet", "Haiku (quickest)")
        AlertDialog.Builder(this).setTitle("Claude model").setSingleChoiceItems(labels, keys.indexOf(prefs.model).coerceAtLeast(0)) { d, i ->
            prefs.model = keys[i]
            modelBtn.text = modelLabel()
            d.dismiss()
        }.show()
    }

    private fun pickChat() {
        if (jobId != null) {
            toast("Claude is still working on this chat")
            return
        }
        val list = chats()
        val labels = listOf("New chat") + list.map {
            val n = it.optInt("requests")
            "${it.optString("title").ifEmpty { "Chat" }}\n${Ui.ago(it.optLong("updated"))} · $n request${if (n == 1) "" else "s"}"
        }
        AlertDialog.Builder(this).setTitle("Chats about this document").setItems(labels.toTypedArray()) { _, i ->
            openChat(if (i == 0) null else list[i - 1].optString("id"))
        }.show()
    }

    private fun openChat(id: String?) {
        chatId = id
        prefs.setChatFor(docId, id)
        msgs.removeAllViews()
        resetStream()
        chatBtn.text = (id?.let { cid -> chats().firstOrNull { it.optString("id") == cid }?.optString("title") } ?: "New chat")
        if (id == null) {
            notice("Claude runs on netdash with your Claude Code sign-in, and edits this document there. The chat is saved with the document, so you can carry on in rCAD on the computer too.", false)
            return
        }
        val a = api ?: return
        Thread {
            try {
                val c = a.get("/api/doc/${Api.enc(docId)}/chats/${Api.enc(id)}")
                val lines = c.optJSONArray("lines")
                runOnUiThread {
                    for (i in 0 until (lines?.length() ?: 0)) showLine(lines!!.getJSONObject(i))
                    resetStream()
                    scrollDown()
                }
            } catch (e: IOException) {
                runOnUiThread { notice("Couldn't open that chat: ${e.message}", true) }
            }
        }.start()
    }

    private fun showLine(l: JSONObject) {
        when (l.optString("kind")) {
            "user" -> userBubble(l.optString("text"))
            "assistant" -> assistantText(l.optString("text"))
            "thinking" -> {}
            "tool" -> {
                val ok = if (l.isNull("ok")) null else l.optBoolean("ok")
                toolRow(l.optString("name"), ok, l.optString("summary"), null)
            }
            "notice" -> notice(l.optString("text"), l.optBoolean("error"))
        }
    }

    // ---------------------------------------------------------------- a request

    private fun sendPrompt() {
        val text = input.text.toString().trim()
        if (text.isEmpty()) return
        val a = api ?: run { load(); toast("Connecting to netdash…"); return }
        input.setText("")
        (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(input.windowToken, 0)
        userBubble(text)
        resetStream()
        setBusy(true, null)
        val body = JSONObject().put("text", text)
        chatId?.let { body.put("chat_id", it) }
        body.put("studio", studio)
        if (prefs.model.isNotEmpty()) body.put("model", prefs.model)
        Thread {
            try {
                val res = a.post("/api/doc/${Api.enc(docId)}/chat", body)
                val jid = res.getString("job")
                runOnUiThread {
                    if (!res.isNull("chat_id")) setChat(res.optString("chat_id"))
                    poll(jid, 0)
                }
            } catch (e: IOException) {
                runOnUiThread {
                    notice(e.message ?: "Couldn't send", true)
                    setBusy(false, null)
                }
            }
        }.start()
    }

    /** Follow a request that is already running (started from this phone earlier, or by another). */
    private fun attach(jid: String) {
        msgs.removeAllViews()
        val a = api ?: return
        Thread {
            try {
                val res = a.get("/api/jobs/$jid?after=0&wait=0")
                runOnUiThread {
                    val cid = if (res.isNull("chat_id")) null else res.optString("chat_id")
                    if (cid != null) setChat(cid)
                    val prompt = res.optString("prompt")
                    if (prompt.isNotEmpty()) userBubble(prompt)
                    resetStream()
                    setBusy(true, jid)
                    poll(jid, 0)
                }
            } catch (e: IOException) {
                runOnUiThread { openChat(chatId) }
            }
        }.start()
    }

    private fun setChat(id: String) {
        chatId = id
        prefs.setChatFor(docId, id)
    }

    private fun setBusy(busy: Boolean, jid: String?) {
        jobId = jid ?: if (busy) "" else null
        send.text = if (busy) "Stop" else "Send"
        send.background = Ui.pressable(Ui.rounded(if (busy) Ui.ERR else Ui.ACCENT, Ui.dp(this, 6).toFloat()), dark = true)
        progressCard.visibility = if (busy) View.VISIBLE else View.GONE
        if (busy) progressText.text = "Claude is working…"
    }

    private fun stopJob() {
        val jid = jobId?.takeIf { it.isNotEmpty() } ?: return
        val a = api ?: return
        send.text = "Stopping…"
        Thread {
            try {
                a.post("/api/jobs/$jid/stop", JSONObject())
            } catch (e: IOException) {
                runOnUiThread { toast("Couldn't reach netdash to stop: ${e.message}") }
            }
        }.start()
    }

    /** Long-poll the request's events until it is done, reconnecting when the network drops. */
    private fun poll(jid: String, from: Int) {
        jobId = jid
        docChanged = false
        Thread {
            var after = from
            var failures = 0
            while (!destroyed) {
                try {
                    val a = api ?: Api.connect(prefs).also { api = it }
                    val res = a.get("/api/jobs/$jid?after=$after", readMs = 40000)
                    failures = 0
                    val evs = res.optJSONArray("events")
                    val done = res.optBoolean("done")
                    after = res.optInt("next", after)
                    runOnUiThread {
                        for (i in 0 until (evs?.length() ?: 0)) event(evs!!.getJSONObject(i))
                        if (done) finishJob()
                    }
                    if (done) return@Thread
                } catch (e: ApiException) {
                    if (e.code == 404) {
                        // netdash restarted: what was saved is in the chat
                        runOnUiThread {
                            setBusy(false, null)
                            load()
                            openChat(chatId)
                        }
                        return@Thread
                    }
                    failures++
                } catch (e: IOException) {
                    failures++
                    api = null
                    if (failures == 3) runOnUiThread { notice("Lost netdash; still trying. Claude keeps working there.", true) }
                }
                try {
                    Thread.sleep((1000L * failures).coerceAtMost(10000))
                } catch (e: InterruptedException) {
                    return@Thread
                }
            }
        }.start()
    }

    private fun event(e: JSONObject) {
        when (e.optString("type")) {
            "chat" -> setChat(e.optString("id"))
            "text" -> {
                thinking = null
                val tv = assistant ?: assistantText("").also { assistant = it }
                tv.append(e.optString("text"))
            }
            "thinking" -> {
                val tv = thinking ?: Ui.text(this, "", 12.5f, Ui.MUTED).also {
                    it.setTypeface(it.typeface, android.graphics.Typeface.ITALIC)
                    it.maxLines = 3
                    it.ellipsize = android.text.TextUtils.TruncateAt.END
                    it.setOnClickListener { _ -> it.maxLines = if (it.maxLines == 3) 200 else 3 }
                    msgs.addView(it)
                    thinking = it
                }
                tv.append(e.optString("text"))
            }
            "tool_start" -> {
                assistant = null
                thinking = null
                progressText.text = "Claude is working… ${e.optString("name").replace('_', ' ')}"
                toolRow(e.optString("name"), null, "", null)
            }
            "tool_done" -> {
                val ok = e.optBoolean("ok")
                val img = e.optString("image").takeIf { it.isNotEmpty() }
                val tv = pendingTool
                if (tv != null) {
                    tv.text = toolText(tv.tag as? String ?: "", ok, e.optString("summary"))
                    tv.setTextColor(if (ok) Ui.MUTED else Ui.ERR)
                    if (img != null) pendingToolBox?.let { addImage(it, img) }
                    pendingTool = null
                } else {
                    toolRow("tool", ok, e.optString("summary"), img)
                }
            }
            "notice" -> notice(e.optString("text"), false)
            "attempt" -> {
                attemptAt = msgs.childCount
                attemptLen = (msgs.getChildAt(msgs.childCount - 1) as? TextView)?.takeIf { it === assistant || it === thinking }?.text?.length
            }
            "rewind" -> {
                while (msgs.childCount > attemptAt) msgs.removeViewAt(msgs.childCount - 1)
                val last = msgs.getChildAt(msgs.childCount - 1) as? TextView
                val len = attemptLen
                if (last != null && len != null && (last === assistant || last === thinking)) last.text = last.text.subSequence(0, len.coerceAtMost(last.text.length))
                else {
                    assistant = null
                    thinking = null
                }
            }
            "saved" -> docChanged = true
            "done" -> {
                val err = if (e.isNull("error")) null else e.optString("error")
                if (err != null) notice(if (err == "Stopped") "Stopped. What Claude changed so far is kept." else err, err != "Stopped")
            }
        }
        scrollDown()
    }

    private fun finishJob() {
        setBusy(false, null)
        resetStream()
        if (docChanged) reloadViewer()
        load()
        // the chat list label follows the new title
        chatBtn.postDelayed({ chatBtn.text = (chatId?.let { cid -> chats().firstOrNull { it.optString("id") == cid }?.optString("title") } ?: "New chat") }, 1500)
    }

    private fun resetStream() {
        assistant = null
        thinking = null
        pendingTool = null
        pendingToolBox = null
    }

    // ---------------------------------------------------------------- message views

    private fun scrollDown() = msgScroll.post { msgScroll.fullScroll(View.FOCUS_DOWN) }

    private fun lp(top: Int = 6) = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = Ui.dp(this@DocActivity, top) }

    private fun userBubble(t: String) {
        msgs.addView(Ui.text(this, t, 15f).apply {
            background = Ui.rounded(Ui.USER_BUBBLE, Ui.dp(context, 8).toFloat())
            setPadding(Ui.dp(context, 12), Ui.dp(context, 8), Ui.dp(context, 12), Ui.dp(context, 8))
            setTextIsSelectable(true)
        }, lp(10))
    }

    private fun assistantText(t: String): TextView {
        val tv = Ui.text(this, t.trim(), 15f).apply { setTextIsSelectable(true) }
        msgs.addView(tv, lp())
        return tv
    }

    private fun toolText(name: String, ok: Boolean?, summary: String): String {
        val mark = when (ok) { null -> "…"; true -> "✓"; false -> "✗" }
        return if (summary.isEmpty()) "$mark $name" else "$mark $name  ${summary.take(160)}"
    }

    private fun toolRow(name: String, ok: Boolean?, summary: String, image: String?) {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val tv = Ui.text(this, toolText(name, ok, summary), 12.5f, if (ok == false) Ui.ERR else Ui.MUTED).apply {
            tag = name
            maxLines = 2
            ellipsize = android.text.TextUtils.TruncateAt.END
        }
        box.addView(tv)
        if (image != null) addImage(box, image)
        msgs.addView(box, lp(2))
        if (ok == null) {
            pendingTool = tv
            pendingToolBox = box
        }
    }

    private fun addImage(box: LinearLayout, b64: String) {
        val bytes = try { Base64.decode(b64, Base64.DEFAULT) } catch (e: IllegalArgumentException) { return }
        val bmp = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return
        val w = Ui.dp(this, 220)
        box.addView(ImageView(this).apply {
            setImageBitmap(bmp)
            adjustViewBounds = true
            scaleType = ImageView.ScaleType.FIT_START
        }, LinearLayout.LayoutParams(w, (w * bmp.height / bmp.width.coerceAtLeast(1)).coerceAtMost(w * 2)).apply { topMargin = Ui.dp(this@DocActivity, 4) })
    }

    private fun notice(t: String, error: Boolean) {
        msgs.addView(Ui.text(this, t, 12.5f, if (error) Ui.ERR else Ui.MUTED), lp())
    }
}
