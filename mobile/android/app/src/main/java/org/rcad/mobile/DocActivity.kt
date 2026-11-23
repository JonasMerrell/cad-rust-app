package org.rcad.mobile

import android.annotation.SuppressLint
import android.app.Activity
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
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import org.json.JSONArray
import org.json.JSONObject
import java.io.IOException

/**
 * One document, laid out like the OrcaSlicer fork's G-code page: rCAD's web viewer fills the
 * screen (made on netdash with export_web, in its app mode), with floating panels over it: the
 * document and tab at the top left, and at the bottom a panel with the feature tree and the chat
 * with Claude, which runs on netdash through Claude Code. Chats are the ones the editor's chat
 * panel saves next to the document, so a conversation can go on in either place.
 */
class DocActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var docId: String
    private var api: Api? = null
    private var detail: JSONObject? = null
    private var tree: JSONObject? = null

    private var assembly: Int? = null
    private var studio: Int = 0
    private var chatId: String? = null
    private var jobId: String? = null
    private var destroyed = false
    /** 0 the feature tree, 1 the chat. */
    private var tabShown = 0
    private var sheetOpen = true

    private lateinit var web: WebView
    private lateinit var webNote: TextView
    private lateinit var info: LinearLayout
    private lateinit var subtitle: TextView
    private lateinit var stats: TextView
    private lateinit var sheet: LinearLayout
    private lateinit var sheetBody: FrameLayout
    private lateinit var treeScroll: ScrollView
    private lateinit var treeList: LinearLayout
    private lateinit var chatBox: LinearLayout
    private lateinit var tabTree: TextView
    private lateinit var tabChat: TextView
    private lateinit var hideBtn: Button
    private lateinit var msgs: LinearLayout
    private lateinit var msgScroll: ScrollView
    private lateinit var input: EditText
    private lateinit var send: Button
    private lateinit var chatBtn: Button
    private lateinit var modelBtn: Button

    // the streaming reply
    private var assistant: TextView? = null
    private var thinking: TextView? = null
    private var pendingTool: TextView? = null
    private var pendingToolBox: LinearLayout? = null
    private var attemptAt = 0
    private var attemptLen: Int? = null
    private var docChanged = false

    private fun mp() = ViewGroup.LayoutParams.MATCH_PARENT
    private fun wc() = ViewGroup.LayoutParams.WRAP_CONTENT

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Ui.init(this)
        prefs = Prefs(this)
        docId = intent.getStringExtra("doc") ?: run { finish(); return }
        chatId = prefs.chatFor(docId)
        val name = intent.getStringExtra("name") ?: "Document"
        val pad = Ui.dp(this, 12)

        // the model, full screen
        web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            setBackgroundColor(Ui.BG)
            webViewClient = object : WebViewClient() {
                override fun onPageFinished(view: WebView?, url: String?) {
                    if (url?.startsWith("http") == true) {
                        webNote.visibility = View.GONE
                        pushInsets()
                    }
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
        webNote = Ui.text(this, "Building the 3D view on netdash…", 15f, Ui.MUTED).apply {
            gravity = Gravity.CENTER
            setPadding(pad * 2, pad * 2, pad * 2, pad * 2)
        }

        // top left: the document (Orca's info panel)
        val back = Ui.text(this, "‹", 26f, Ui.TEXT).apply {
            setPadding(0, 0, Ui.dp(context, 10), Ui.dp(context, 2))
            setOnClickListener { finish() }
        }
        val title = Ui.text(this, name, 15f, bold = true).apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
        }
        subtitle = Ui.text(this, "Part Studio ▾", 13f, Ui.ACCENT).apply { setOnClickListener { pickTab() } }
        stats = Ui.text(this, "", 13f, Ui.MUTED)
        val texts = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(title)
            addView(subtitle)
            addView(stats)
        }
        info = Ui.row(this, back, texts).apply {
            background = Ui.panel(context)
            setPadding(Ui.dp(context, 10), Ui.dp(context, 8), pad, Ui.dp(context, 8))
            gravity = Gravity.CENTER_VERTICAL
        }

        // bottom: feature tree and chat (Orca's side panel, which docks at the bottom on phones)
        tabTree = Ui.tab(this, "Feature tree") { showTab(0) }
        tabChat = Ui.tab(this, "Claude") { showTab(1) }
        hideBtn = Ui.button(this, "Hide") { setSheet(!sheetOpen) }
        val spacer = View(this).apply { layoutParams = LinearLayout.LayoutParams(0, 1, 1f) }
        val tabs = Ui.row(this, tabTree, tabChat, spacer, hideBtn).apply { setPadding(Ui.dp(context, 4), Ui.dp(context, 4), Ui.dp(context, 8), Ui.dp(context, 4)) }

        treeList = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, 0, pad, pad)
        }
        treeScroll = ScrollView(this).apply { addView(treeList) }

        chatBtn = Ui.button(this, "New chat ▾") { pickChat() }.apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(0, wc(), 1f)
        }
        modelBtn = Ui.button(this, modelLabel()) { pickModel() }
        val chatBar = Ui.row(this, chatBtn, gap(), modelBtn).apply { setPadding(pad, Ui.dp(context, 2), pad, Ui.dp(context, 4)) }
        msgs = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, 0, pad, Ui.dp(context, 8))
        }
        msgScroll = ScrollView(this).apply { addView(msgs) }
        input = EditText(this).apply {
            hint = "Ask Claude to design or change something"
            setTextColor(Ui.TEXT)
            setHintTextColor(Ui.MUTED)
            textSize = 14.5f
            maxLines = 5
            background = Ui.rounded(Ui.BG, Ui.dp(context, 6).toFloat(), Ui.BORDER, maxOf(1, Ui.dp(context, 1)))
            setPadding(Ui.dp(context, 10), Ui.dp(context, 7), Ui.dp(context, 10), Ui.dp(context, 7))
            layoutParams = LinearLayout.LayoutParams(0, wc(), 1f)
        }
        send = Ui.button(this, "Send", primary = true) { if (jobId != null) stopJob() else sendPrompt() }
        val inputRow = Ui.row(this, input, gap(), send).apply { setPadding(pad, Ui.dp(context, 4), pad, pad) }
        chatBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(chatBar)
            addView(msgScroll, LinearLayout.LayoutParams(mp(), 0, 1f))
            addView(inputRow)
        }
        sheetBody = FrameLayout(this).apply {
            addView(treeScroll, FrameLayout.LayoutParams(mp(), mp()))
            addView(chatBox, FrameLayout.LayoutParams(mp(), mp()))
        }
        sheet = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = Ui.panel(context)
            addView(tabs)
            addView(Ui.rule(context).apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 })
            addView(sheetBody, LinearLayout.LayoutParams(mp(), 0, 1f))
        }

        val root = FrameLayout(this).apply {
            setBackgroundColor(Ui.BG)
            fitsSystemWindows = true
            addView(web, FrameLayout.LayoutParams(mp(), mp()))
            addView(webNote, FrameLayout.LayoutParams(mp(), mp()))
            addView(info, FrameLayout.LayoutParams(wc(), wc(), Gravity.TOP or Gravity.START).apply {
                setMargins(pad, pad, pad, 0)
            })
            addView(sheet, FrameLayout.LayoutParams(mp(), wc(), Gravity.BOTTOM).apply { setMargins(pad, 0, pad, pad) })
        }
        setContentView(root)
        // the panel takes about 42% of the height when open (more while typing, as the keyboard shrinks the window)
        root.addOnLayoutChangeListener { _, _, top, _, bottom, _, oldTop, _, oldBottom ->
            if (bottom - top != oldBottom - oldTop) sizeSheet(bottom - top)
        }
        info.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> pushInsets() }
        showTab(if (chatId != null) 1 else 0)
        load(first = true)
    }

    override fun onDestroy() {
        destroyed = true
        if (::web.isInitialized) web.destroy()
        super.onDestroy()
    }

    private fun gap() = View(this).apply { layoutParams = LinearLayout.LayoutParams(Ui.dp(context, 6), 1) }

    private fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()

    private fun showWebNote(t: String) {
        webNote.text = t
        webNote.visibility = View.VISIBLE
    }

    // ---------------------------------------------------------------- layout

    private fun sizeSheet(h: Int) {
        val lp = sheet.layoutParams as FrameLayout.LayoutParams
        lp.height = if (sheetOpen) (h * 0.42f).toInt().coerceAtLeast(Ui.dp(this, 220)) else ViewGroup.LayoutParams.WRAP_CONTENT
        sheet.layoutParams = lp
        sheet.post { pushInsets() }
    }

    private fun setSheet(open: Boolean) {
        sheetOpen = open
        sheetBody.visibility = if (open) View.VISIBLE else View.GONE
        hideBtn.text = if (open) "Hide" else "Show"
        sizeSheet((sheet.parent as View).height)
    }

    private fun showTab(i: Int) {
        tabShown = i
        Ui.setTab(tabTree, i == 0)
        Ui.setTab(tabChat, i == 1)
        treeScroll.visibility = if (i == 0) View.VISIBLE else View.GONE
        chatBox.visibility = if (i == 1) View.VISIBLE else View.GONE
        if (!sheetOpen) setSheet(true)
    }

    /** Tell the page how much of the screen the panels cover, so its own buttons stay clear of them. */
    private fun pushInsets() {
        if (!::web.isInitialized) return
        val d = resources.displayMetrics.density
        val top = (info.bottom / d).toInt()
        val bottom = ((web.height - sheet.top) / d).toInt().coerceAtLeast(0)
        web.evaluateJavascript(
            "(function(r){r.style.setProperty('--app-top','${top - 12}px');r.style.setProperty('--app-bottom','${bottom}px');})(document.documentElement)",
            null,
        )
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
                    updateTabLabel()
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

    private fun updateTabLabel() {
        val n = assembly?.let { names("assemblies").getOrNull(it) } ?: names("studios").getOrNull(studio) ?: "Part Studio"
        val kind = if (assembly != null) "Assembly" else "Part Studio"
        val many = names("studios").size + names("assemblies").size > 1
        subtitle.text = "$kind · $n" + if (many) "  ▾" else ""
    }

    /** Build (or fetch from the cache) the page and its feature tree, then show both. */
    private fun reloadViewer() {
        val a = api ?: return
        showWebNote("Building the 3D view on netdash…\n(a big model can take a minute)")
        treeList.removeAllViews()
        treeList.addView(Ui.text(this, "Loading the feature tree…", 13.5f, Ui.MUTED).apply { setPadding(0, Ui.dp(context, 8), 0, 0) })
        val sel = if (assembly != null) "assembly=$assembly" else "studio=$studio"
        val want = Pair(assembly, studio)
        Thread {
            try {
                val t = a.get("/api/doc/${Api.enc(docId)}/tree?$sel", readMs = 16 * 60 * 1000)
                runOnUiThread {
                    if (want != Pair(assembly, studio)) return@runOnUiThread
                    tree = t
                    showTree(t)
                    web.loadUrl(a.viewUrl(docId, assembly, studio) + "&app=1")
                }
            } catch (e: IOException) {
                runOnUiThread {
                    showWebNote("Couldn't show the model: ${e.message}")
                    treeList.removeAllViews()
                    treeList.addView(Ui.text(this, e.message ?: "Couldn't load the feature tree", 13.5f, Ui.ERR))
                }
            }
        }.start()
    }

    private fun pickTab() {
        val studios = names("studios")
        val asms = names("assemblies")
        val labels = studios.map { "Part Studio · $it" } + asms.map { "Assembly · $it" }
        if (labels.size < 2) return
        Ui.dialog(this).setTitle("Show").setItems(labels.toTypedArray()) { _, i ->
            if (i < studios.size) {
                studio = i
                assembly = null
            } else {
                assembly = i - studios.size
            }
            updateTabLabel()
            reloadViewer()
        }.show()
    }

    // ---------------------------------------------------------------- feature tree

    private fun objs(a: JSONArray?): List<JSONObject> = (0 until (a?.length() ?: 0)).mapNotNull { a!!.optJSONObject(it) }

    private fun showTree(t: JSONObject) {
        treeList.removeAllViews()
        if (t.optString("kind") == "assembly") {
            val inst = objs(t.optJSONArray("instances"))
            val mates = objs(t.optJSONArray("mates"))
            val failed = t.optJSONArray("failed_mates")?.length() ?: 0
            stats.text = "${inst.size} instance${if (inst.size == 1) "" else "s"} · ${mates.size} mate${if (mates.size == 1) "" else "s"}" +
                if (failed > 0) " · $failed failed" else ""
            stats.setTextColor(if (failed > 0) Ui.ERR else Ui.MUTED)
            treeList.addView(Ui.title(this, "Instances"))
            for (i in inst) {
                val mark = if (i.optBoolean("sub")) "▣" else "◧"
                val sub = listOfNotNull("fixed".takeIf { i.optBoolean("fixed") }, "hidden".takeIf { i.optBoolean("hidden") }).joinToString(", ")
                treeList.addView(treeRow(mark, i.optString("name"), sub, dim = i.optBoolean("hidden"), error = null) {
                    askAbout("instance \u201c${i.optString("name")}\u201d")
                })
            }
            if (mates.isNotEmpty()) {
                treeList.addView(Ui.title(this, "Mates"))
                for (m in mates) treeList.addView(treeRow("⛓", m.optString("name"), m.optString("kind"), dim = false, error = null) {
                    askAbout("mate \u201c${m.optString("name")}\u201d")
                })
            }
            return
        }
        val feats = objs(t.optJSONArray("features"))
        val parts = objs(t.optJSONArray("parts"))
        val errs = feats.count { it.has("error") }
        stats.text = "${parts.size} part${if (parts.size == 1) "" else "s"} · ${feats.size} feature${if (feats.size == 1) "" else "s"}" +
            if (errs > 0) " · $errs failing" else ""
        stats.setTextColor(if (errs > 0) Ui.ERR else Ui.MUTED)
        if (feats.isEmpty()) {
            treeList.addView(Ui.text(this, "No features yet. Ask Claude to design something.", 13.5f, Ui.MUTED).apply { setPadding(0, Ui.dp(context, 8), 0, 0) })
        }
        if (feats.isNotEmpty()) treeList.addView(Ui.title(this, "Features"))
        for (f in feats) {
            val err = if (f.has("error")) f.optString("error") else null
            treeList.addView(treeRow(Ui.featureIcon(f.optString("kind")), f.optString("name"), f.optString("detail"), dim = f.optBoolean("suppressed"), error = err) {
                featureDialog(f)
            })
        }
        val vars = objs(t.optJSONArray("variables"))
        if (vars.isNotEmpty()) {
            treeList.addView(Ui.rule(this))
            treeList.addView(Ui.title(this, "Variables"))
            for (v in vars) treeList.addView(treeRow("#", v.optString("name"), "= " + v.optString("expr"), dim = false, error = null) {
                askAbout("variable ${v.optString("name")} (now ${v.optString("expr")})")
            })
        }
        if (parts.isNotEmpty()) {
            treeList.addView(Ui.rule(this))
            treeList.addView(Ui.title(this, "Parts"))
            for (p in parts) {
                val vol = p.optDouble("volume_mm3", Double.NaN)
                val sub = if (vol.isNaN()) "" else if (vol >= 1000) "%.1f cm³".format(vol / 1000) else "%.0f mm³".format(vol)
                treeList.addView(treeRow("◆", p.optString("name"), sub, dim = false, error = null) { askAbout("part \u201c${p.optString("name")}\u201d") })
            }
        }
    }

    private fun treeRow(mark: String, name: String, detail: String, dim: Boolean, error: String?, onTap: () -> Unit): View {
        val color = when {
            error != null -> Ui.ERR
            dim -> Ui.MUTED
            else -> Ui.TEXT
        }
        val icon = Ui.text(this, mark, 14f, if (error != null) Ui.ERR else Ui.ACCENT).apply {
            gravity = Gravity.CENTER
            layoutParams = LinearLayout.LayoutParams(Ui.dp(context, 22), wc())
        }
        val label = Ui.text(this, name, 14f, color).apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
            if (dim) paintFlags = paintFlags or Paint.STRIKE_THRU_TEXT_FLAG
        }
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, wc(), 1f)
            addView(label)
            val sub = error ?: detail
            if (sub.isNotEmpty()) addView(Ui.text(context, sub, 12f, if (error != null) Ui.ERR else Ui.MUTED).apply {
                maxLines = 1
                ellipsize = android.text.TextUtils.TruncateAt.END
            })
        }
        return Ui.row(this, icon, gap(), col).apply {
            setPadding(0, Ui.dp(context, 5), 0, Ui.dp(context, 5))
            isClickable = true
            setOnClickListener { onTap() }
        }
    }

    private fun featureDialog(f: JSONObject) {
        val lines = mutableListOf<String>()
        lines += f.optString("kind")
        if (f.optString("detail").isNotEmpty()) lines += f.optString("detail")
        if (f.optBoolean("suppressed")) lines += "Suppressed"
        if (f.has("error")) lines += "Error: " + f.optString("error")
        val name = f.optString("name")
        Ui.dialog(this).setTitle(name).setMessage(lines.joinToString("\n"))
            .setPositiveButton("Ask Claude about it") { _, _ -> askAbout("feature \u201c$name\u201d") }
            .setNegativeButton("Close", null)
            .show()
    }

    /** Start a request about something in the tree: switch to the chat with its name typed. */
    private fun askAbout(what: String) {
        showTab(1)
        val cur = input.text.toString().trim()
        input.setText(if (cur.isEmpty()) "In the $what, " else "$cur (the $what) ")
        input.setSelection(input.text.length)
        input.requestFocus()
        (getSystemService(INPUT_METHOD_SERVICE) as InputMethodManager).showSoftInput(input, 0)
    }

    // ---------------------------------------------------------------- chats

    private fun modelLabel() = when (prefs.model) {
        "opus" -> "Opus ▾"
        "sonnet" -> "Sonnet ▾"
        "haiku" -> "Haiku ▾"
        else -> "Model ▾"
    }

    private fun pickModel() {
        val keys = listOf("", "opus", "sonnet", "haiku")
        val labels = arrayOf("As set in rCAD on netdash", "Opus (plans, hands work to Sonnet/Haiku)", "Sonnet", "Haiku (quickest)")
        Ui.dialog(this).setTitle("Claude model").setSingleChoiceItems(labels, keys.indexOf(prefs.model).coerceAtLeast(0)) { d, i ->
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
        Ui.dialog(this).setTitle("Chats about this document").setItems(labels.toTypedArray()) { _, i ->
            openChat(if (i == 0) null else list[i - 1].optString("id"))
        }.show()
    }

    private fun openChat(id: String?) {
        chatId = id
        prefs.setChatFor(docId, id)
        msgs.removeAllViews()
        resetStream()
        chatBtn.text = (id?.let { cid -> chats().firstOrNull { it.optString("id") == cid }?.optString("title") } ?: "New chat") + " ▾"
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
        showTab(1)
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
        send.background = Ui.rounded(if (busy) Ui.ERR else Ui.ACCENT, Ui.dp(this, 10).toFloat())
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
        chatBtn.postDelayed({ chatBtn.text = (chatId?.let { cid -> chats().firstOrNull { it.optString("id") == cid }?.optString("title") } ?: "New chat") + " ▾" }, 1500)
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
            background = Ui.rounded(Ui.USER_BG, Ui.dp(context, 8).toFloat())
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
