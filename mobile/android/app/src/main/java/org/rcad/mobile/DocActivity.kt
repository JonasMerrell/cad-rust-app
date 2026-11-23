package org.rcad.mobile

import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.graphics.BitmapFactory
import android.os.Bundle
import android.util.Base64
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
import org.json.JSONObject
import java.io.IOException

/**
 * One document: rCAD's own web viewer (the page export_web writes, made on netdash) above a chat
 * with Claude, which runs on netdash through Claude Code. Chats are the same ones the editor's
 * chat panel saves next to the document, so a conversation can go on in either place.
 */
class DocActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var docId: String
    private var api: Api? = null
    private var detail: JSONObject? = null

    private var assembly: Int? = null
    private var studio: Int = 0
    private var chatId: String? = null
    private var jobId: String? = null
    private var destroyed = false
    /** 0 viewer above chat, 1 viewer only, 2 chat only. */
    private var layoutMode = 0

    private lateinit var web: WebView
    private lateinit var webBox: FrameLayout
    private lateinit var webNote: TextView
    private lateinit var chatBox: LinearLayout
    private lateinit var msgs: LinearLayout
    private lateinit var msgScroll: ScrollView
    private lateinit var input: EditText
    private lateinit var send: Button
    private lateinit var tabBtn: Button
    private lateinit var chatBtn: Button
    private lateinit var modelBtn: Button
    private lateinit var layoutBtn: Button

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

        val title = Ui.text(this, name, 17f, bold = true).apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        tabBtn = Ui.button(this, "Part Studio ▾") { pickTab() }
        layoutBtn = Ui.button(this, "⇕") { cycleLayout() }
        val bar = Ui.row(this, Ui.button(this, "‹") { finish() }, gap(), title, gap(), tabBtn, gap(), layoutBtn)
            .apply { setPadding(Ui.dp(context, 10), Ui.dp(context, 8), Ui.dp(context, 10), Ui.dp(context, 8)) }

        web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.loadWithOverviewMode = true
            settings.useWideViewPort = true
            setBackgroundColor(Ui.BG)
            webViewClient = object : WebViewClient() {
                override fun onPageFinished(view: WebView?, url: String?) {
                    if (url?.startsWith("http") == true) webNote.visibility = View.GONE
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
            setPadding(Ui.dp(context, 20), Ui.dp(context, 20), Ui.dp(context, 20), Ui.dp(context, 20))
            setBackgroundColor(Ui.BG)
            gravity = android.view.Gravity.CENTER
        }
        webBox = FrameLayout(this).apply {
            addView(web, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
            addView(webNote, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        }

        chatBtn = Ui.button(this, "New chat ▾") { pickChat() }.apply {
            maxLines = 1
            ellipsize = android.text.TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        modelBtn = Ui.button(this, modelLabel()) { pickModel() }
        val chatBar = Ui.row(this, chatBtn, gap(), modelBtn).apply { setPadding(Ui.dp(context, 10), Ui.dp(context, 6), Ui.dp(context, 10), Ui.dp(context, 4)) }
        msgs = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(Ui.dp(context, 12), Ui.dp(context, 4), Ui.dp(context, 12), Ui.dp(context, 12))
        }
        msgScroll = ScrollView(this).apply { addView(msgs) }
        input = EditText(this).apply {
            hint = "Ask Claude to design or change something"
            setTextColor(Ui.TEXT)
            setHintTextColor(Ui.MUTED)
            textSize = 15f
            maxLines = 5
            background = Ui.rounded(Ui.PANEL, Ui.dp(context, 10).toFloat())
            setPadding(Ui.dp(context, 12), Ui.dp(context, 8), Ui.dp(context, 12), Ui.dp(context, 8))
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        send = Ui.button(this, "Send", primary = true) { if (jobId != null) stopJob() else sendPrompt() }
        val inputRow = Ui.row(this, input, gap(), send).apply { setPadding(Ui.dp(context, 10), Ui.dp(context, 6), Ui.dp(context, 10), Ui.dp(context, 10)) }
        chatBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.PANEL)
            addView(chatBar)
            addView(msgScroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(inputRow)
        }

        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.BG)
            fitsSystemWindows = true
            addView(bar)
            addView(webBox, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1.15f))
            addView(chatBox, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        })
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
        tabBtn.text = (assembly?.let { names("assemblies").getOrNull(it) } ?: names("studios").getOrNull(studio) ?: "Part Studio") + " ▾"
    }

    private fun reloadViewer() {
        val a = api ?: return
        showWebNote("Building the 3D view on netdash… (a big model can take a minute)")
        web.loadUrl(a.viewUrl(docId, assembly, studio))
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
            updateTabLabel()
            reloadViewer()
        }.show()
    }

    private fun cycleLayout() {
        layoutMode = (layoutMode + 1) % 3
        webBox.visibility = if (layoutMode == 2) View.GONE else View.VISIBLE
        chatBox.visibility = if (layoutMode == 1) View.GONE else View.VISIBLE
        layoutBtn.text = when (layoutMode) { 1 -> "▤"; 2 -> "▣"; else -> "⇕" }
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
            background = Ui.rounded(0xFF243A5A.toInt(), Ui.dp(context, 10).toFloat())
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
