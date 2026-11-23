package org.rcad.mobile

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.text.InputType
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import org.json.JSONObject
import java.io.IOException

/** The documents on netdash: tap one to view it and chat with Claude about it. */
class MainActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var list: LinearLayout
    private lateinit var status: TextView
    private lateinit var search: EditText
    private lateinit var header: TextView
    private var docs: List<JSONObject> = emptyList()
    private var loading = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        val logo = android.widget.ImageView(this).apply {
            setImageResource(R.drawable.ic_launcher)
            layoutParams = LinearLayout.LayoutParams(Ui.dp(context, 30), Ui.dp(context, 30)).apply { marginStart = Ui.dp(context, 4); marginEnd = Ui.dp(context, 10) }
        }
        val title = Ui.text(this, "rCAD", 19f, Ui.BAR_TEXT, bold = true).apply { layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f) }
        val bar = Ui.topBar(
            this, logo, title,
            Ui.accentButton(this, "New project") { newProjectDialog() },
            Ui.iconButton(this, "more", Ui.BAR_TEXT, desc = "More") { v -> moreMenu(v) },
        ).apply { layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(context, 52)) }
        status = Ui.text(this, "", 12.5f, Ui.MUTED).apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 8), Ui.dp(context, 14), Ui.dp(context, 4)) }
        search = EditText(this).apply {
            hint = "Search documents"
            setSingleLine()
            textSize = 14f
            setTextColor(Ui.TEXT)
            setHintTextColor(Ui.FAINT)
            background = Ui.rounded(Ui.SHEET, Ui.dp(context, 4).toFloat(), Ui.LINE, Ui.dp(context, 1))
            setCompoundDrawablesRelative(Icon(context, "search", Ui.FAINT, 18), null, null, null)
            compoundDrawablePadding = Ui.dp(context, 8)
            setPadding(Ui.dp(context, 10), Ui.dp(context, 9), Ui.dp(context, 10), Ui.dp(context, 9))
            addTextChangedListener(object : android.text.TextWatcher {
                override fun afterTextChanged(s: android.text.Editable?) = show()
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            })
        }
        val searchWrap = LinearLayout(this).apply {
            setBackgroundColor(Ui.SHEET)
            setPadding(Ui.dp(context, 12), Ui.dp(context, 10), Ui.dp(context, 12), Ui.dp(context, 10))
            addView(search, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        }
        header = Ui.text(this, "", 12.5f, Ui.MUTED)
        list = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.SHEET)
        }
        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.SHEET)
            setPadding(0, 0, 0, Ui.dp(context, 24))
            addView(Ui.sectionRow(context, "folder", "Documents", header).apply {
                (layoutParams as LinearLayout.LayoutParams).topMargin = 0
            })
            addView(searchWrap)
            addView(Ui.divider(context))
            addView(list)
        }
        val scroll = ScrollView(this).apply {
            setBackgroundColor(Ui.SHEET)
            isFillViewport = true
            addView(body)
        }
        setContentView(Ui.screen(this, LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(bar)
            addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                setBackgroundColor(Ui.SCENE)
                addView(status)
                addView(scroll, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        }))
        handlePairing(intent)
    }

    private fun moreMenu(anchor: View) {
        val m = android.widget.PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Refresh")
        m.menu.add(0, 2, 1, "Settings")
        m.setOnMenuItemClickListener {
            if (it.itemId == 1) refresh() else settingsDialog()
            true
        }
        m.show()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handlePairing(intent)
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    private fun handlePairing(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme != "rcad" || uri.host != "pair") return
        val urls = uri.getQueryParameter("url").orEmpty()
        val token = uri.getQueryParameter("token").orEmpty()
        if (urls.isBlank() || token.isBlank()) {
            toast("That pairing link is missing the address or token")
            return
        }
        prefs.urls = urls.split(',').map { it.trim() }
        prefs.token = token
        prefs.lastGoodUrl = ""
        toast("Paired with ${prefs.urls.first()}")
        refresh()
    }

    private fun refresh() {
        if (loading) return
        if (!prefs.paired) {
            status.visibility = View.VISIBLE
            status.text = "Not paired. On the phone, open the pairing link from netdash (rcad_capture_receiver.py pair-url) and tap Open in rCAD, or fill in Settings."
            return
        }
        loading = true
        status.text = "Loading from netdash…"
        status.visibility = View.VISIBLE
        Thread {
            try {
                val api = Api.connect(prefs)
                val res = api.get("/api/projects")
                val arr = res.optJSONArray("documents")
                val out = (0 until (arr?.length() ?: 0)).map { arr!!.getJSONObject(it) }
                val notes = mutableListOf<String>()
                if (!res.optBoolean("rcad", true)) notes += "rcad isn't installed on netdash, so models can't be shown"
                if (!res.optBoolean("claude", true)) notes += "Claude Code isn't on netdash's PATH, so chats won't work"
                runOnUiThread {
                    docs = out
                    header.text = "${out.size} on ${api.base.removePrefix("http://")}"
                    status.text = notes.joinToString(". ")
                    status.visibility = if (notes.isEmpty()) View.GONE else View.VISIBLE
                    show()
                }
            } catch (e: IOException) {
                runOnUiThread {
                    status.visibility = View.VISIBLE
                    status.text = e.message ?: "Can't reach netdash"
                }
            } finally {
                runOnUiThread { loading = false }
            }
        }.start()
    }

    private fun show() {
        list.removeAllViews()
        val q = search.text.toString().trim().lowercase()
        val shown = docs.filter { q.isEmpty() || it.optString("name").lowercase().contains(q) || it.optString("project").lowercase().contains(q) }
        if (shown.isEmpty() && docs.isNotEmpty()) list.addView(Ui.text(this, "No document matches.", 14f, Ui.MUTED).apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 14), 0, 0) })
        if (docs.isEmpty() && prefs.paired && !loading) {
            list.addView(Ui.text(this, "No rCAD documents in netdash's project folders yet. Tap New project to start one, or save a document into rCAD Projects on netdash.", 14f, Ui.MUTED)
                .apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 14), Ui.dp(context, 14), 0) })
        }
        for (d in shown) list.addView(docRow(d))
    }

    private fun docRow(d: JSONObject): View {
        val name = d.optString("name")
        val project = d.optString("project")
        val chats = d.optInt("chats")
        val sub = buildList {
            if (project.isNotEmpty() && project != name) add(project)
            add(Ui.ago(d.optLong("modified")))
            if (chats > 0) add("$chats chat${if (chats == 1) "" else "s"}")
            if (d.optBoolean("git")) add("git")
            if (d.optBoolean("busy")) add("Claude is working")
        }.joinToString(" · ")
        val busy = d.optBoolean("busy")
        val texts = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(Ui.text(context, name, 15.5f, bold = true).apply { isSingleLine = true; ellipsize = android.text.TextUtils.TruncateAt.END })
            addView(Ui.text(context, sub, 12.5f, if (busy) Ui.ACCENT else Ui.MUTED).apply { isSingleLine = true; ellipsize = android.text.TextUtils.TruncateAt.END })
        }
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = android.view.Gravity.CENTER_VERTICAL
            background = Ui.pressable(null)
            setPadding(Ui.dp(context, 14), Ui.dp(context, 11), Ui.dp(context, 10), Ui.dp(context, 11))
            addView(android.widget.ImageView(context).apply { setImageDrawable(Icon(context, "partStudio", Ui.ACCENT, 24)) },
                LinearLayout.LayoutParams(Ui.dp(context, 24), Ui.dp(context, 24)).apply { marginEnd = Ui.dp(context, 12) })
            addView(texts, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(android.widget.ImageView(context).apply { setImageDrawable(Icon(context, "chevronRight", Ui.FAINT, 18)) })
            isClickable = true
            setOnClickListener { open(d.optString("id"), name) }
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(row)
            addView(Ui.divider(context).apply { (layoutParams as LinearLayout.LayoutParams).marginStart = Ui.dp(context, 50) })
        }
    }

    private fun open(id: String, name: String) {
        startActivity(Intent(this, DocActivity::class.java).putExtra("doc", id).putExtra("name", name))
    }

    private fun field(hint: String, value: String = "", type: Int = InputType.TYPE_CLASS_TEXT) = EditText(this).apply {
        this.hint = hint
        setText(value)
        inputType = type
    }

    private fun newProjectDialog() {
        val name = field("Project name, e.g. hex tray")
        AlertDialog.Builder(this)
            .setTitle("New project")
            .setMessage("Makes a folder with an empty document in netdash's rCAD Projects. Then ask Claude to design something.")
            .setView(Ui.column(this, 20, name))
            .setPositiveButton("Create") { _, _ ->
                val n = name.text.toString().trim()
                if (n.isEmpty()) return@setPositiveButton
                Thread {
                    try {
                        val api = Api.connect(prefs)
                        val res = api.post("/api/projects", JSONObject().put("name", n), readMs = 130000)
                        runOnUiThread { open(res.getString("id"), n) }
                    } catch (e: IOException) {
                        runOnUiThread { toast(e.message ?: "Could not make the project") }
                    }
                }.start()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun settingsDialog() {
        val urls = field("netdash address(es), comma separated", prefs.urls.joinToString(", "), InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI)
        val token = field("Token", prefs.token)
        val help = TextView(this).apply {
            text = "The same address and token as rCAD Capture. The pairing link from netdash fills these in. Example address: http://192.168.0.247:8765"
            setPadding(0, 16, 0, 0)
        }
        AlertDialog.Builder(this)
            .setTitle("Settings")
            .setView(Ui.column(this, 20, urls, token, help))
            .setPositiveButton("Save") { _, _ ->
                prefs.urls = urls.text.toString().split(',', ' ', '\n').map { it.trim() }.filter { it.isNotEmpty() }
                    .map { if (it.startsWith("http")) it else "http://$it" }
                prefs.token = token.text.toString()
                prefs.lastGoodUrl = ""
                refresh()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
}
