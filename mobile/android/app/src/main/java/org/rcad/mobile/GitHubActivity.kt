package org.rcad.mobile

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import org.json.JSONObject
import java.io.IOException

/**
 * GitHub on netdash: the account rCAD there is signed in with (sign in with a code, the way TVs
 * do it, so the token stays on netdash), and the account's repositories. Tap one to put a copy on
 * netdash and open it; after that it syncs before and after every chat.
 */
class GitHubActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var account: LinearLayout
    private lateinit var list: LinearLayout
    private lateinit var status: TextView
    private lateinit var search: EditText
    private lateinit var header: TextView
    private var repos: List<JSONObject> = emptyList()
    private var user: JSONObject? = null
    /** Bumped to stop a sign-in's polling. */
    private var signIn = 0
    private var busy = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        val back = Ui.iconButton(this, "chevronRight", Ui.BAR_TEXT, desc = "Back") { finish() }.apply { rotation = 180f }
        val title = Ui.text(this, "GitHub", 17f, Ui.BAR_TEXT, bold = true).apply { layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f) }
        val bar = Ui.topBar(this, back, title, Ui.iconButton(this, "more", Ui.BAR_TEXT, desc = "More") { v -> moreMenu(v) })
            .apply { layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, Ui.dp(context, 52)) }
        status = Ui.text(this, "", 12.5f, Ui.MUTED).apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 8), Ui.dp(context, 14), Ui.dp(context, 4)) }
        account = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.SHEET)
            setPadding(Ui.dp(context, 14), Ui.dp(context, 12), Ui.dp(context, 14), Ui.dp(context, 14))
        }
        search = EditText(this).apply {
            hint = "Search repositories"
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
        header = Ui.text(this, "", 12.5f, Ui.MUTED)
        list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(Ui.SHEET) }
        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Ui.SHEET)
            setPadding(0, 0, 0, Ui.dp(context, 24))
            addView(Ui.sectionRow(context, "branch", "Account").apply { (layoutParams as LinearLayout.LayoutParams).topMargin = 0 })
            addView(account)
            addView(Ui.sectionRow(context, "folder", "Repositories", header))
            addView(LinearLayout(context).apply {
                setBackgroundColor(Ui.SHEET)
                setPadding(Ui.dp(context, 12), Ui.dp(context, 10), Ui.dp(context, 12), Ui.dp(context, 10))
                addView(search, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            })
            addView(Ui.divider(context))
            addView(list)
        }
        setContentView(Ui.screen(this, LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(bar)
            addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                setBackgroundColor(Ui.SCENE)
                addView(status)
                addView(ScrollView(context).apply { setBackgroundColor(Ui.SHEET); isFillViewport = true; addView(body) },
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
            }, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        }))
        loadAccount(fresh = false)
    }

    override fun onDestroy() {
        signIn++
        super.onDestroy()
    }

    private fun moreMenu(anchor: View) {
        val m = android.widget.PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Refresh")
        m.menu.add(0, 2, 1, "Open a repository by address…")
        if (user?.optString("source") == "saved") m.menu.add(0, 3, 2, "Sign out")
        m.setOnMenuItemClickListener {
            when (it.itemId) {
                1 -> loadAccount(fresh = true)
                2 -> addressDialog()
                3 -> signOut()
            }
            true
        }
        m.show()
    }

    private fun note(t: String, err: Boolean = false) {
        status.text = t
        status.setTextColor(if (err) Ui.ERR else Ui.MUTED)
        status.visibility = if (t.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun background(work: (Api) -> Unit, failed: (String) -> Unit = { note(it, err = true) }) {
        Thread {
            try {
                work(Api.connect(prefs))
            } catch (e: IOException) {
                runOnUiThread { failed(e.message ?: "Can't reach netdash") }
            }
        }.start()
    }

    // ---------------------------------------------------------------- account

    private fun loadAccount(fresh: Boolean) {
        note("Asking netdash…")
        background({ api ->
            val u = api.get("/api/github" + if (fresh) "?fresh=1" else "", readMs = 40000)
            runOnUiThread {
                user = u
                note("")
                showAccount()
                if (u.optBoolean("signed_in")) loadRepos() else { repos = emptyList(); show() }
            }
        })
    }

    private fun showAccount() {
        account.removeAllViews()
        val u = user ?: return
        if (u.optBoolean("signed_in")) {
            val name = u.optString("name").takeIf { it.isNotEmpty() && it != "null" }
            val login = u.optString("login")
            account.addView(Ui.text(this, if (name != null) "$name ($login)" else login, 15.5f, bold = true))
            val how = when (u.optString("source")) {
                "gh" -> "Signed in through the GitHub CLI on netdash. Sign in here to use another account."
                "env" -> "Signed in with GITHUB_TOKEN on netdash."
                else -> "Signed in on netdash, for the editor there too."
            }
            account.addView(Ui.text(this, how, 12.5f, Ui.MUTED).apply { setPadding(0, Ui.dp(context, 2), 0, Ui.dp(context, 10)) })
            if (u.optString("source") != "saved") account.addView(Ui.outlineButton(this, "Sign in with another account") { startSignIn() }.wrap())
        } else {
            account.addView(Ui.text(this, "Not signed in", 15.5f, bold = true))
            val err = u.optString("error").takeIf { it.isNotEmpty() && it != "null" }
            account.addView(Ui.text(this, err?.let { "GitHub refused netdash's token: $it" }
                ?: "Sign in so netdash can open your rCAD projects on GitHub and push what you change from the phone. The token stays on netdash.",
                12.5f, if (err != null) Ui.ERR else Ui.MUTED).apply { setPadding(0, Ui.dp(context, 2), 0, Ui.dp(context, 10)) })
            account.addView(Ui.accentButton(this, "Sign in with GitHub") { startSignIn() }.wrap())
        }
    }

    private fun View.wrap() = apply { layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT) }

    /** GitHub's device flow: netdash gets a code, the user enters it on github.com, netdash gets the token. */
    private fun startSignIn() {
        val mine = ++signIn
        note("Asking GitHub for a code…")
        background({ api ->
            val c = api.post("/api/github/login", JSONObject())
            runOnUiThread { if (mine == signIn) showCode(c, mine) }
            var wait = c.optLong("interval", 5).coerceAtLeast(1)
            val until = System.currentTimeMillis() + c.optLong("expires_in", 900) * 1000
            while (mine == signIn && System.currentTimeMillis() < until) {
                Thread.sleep(wait * 1000)
                if (mine != signIn) return@background
                val r = try { api.post("/api/github/login/poll", JSONObject()) } catch (e: ApiException) {
                    runOnUiThread { if (mine == signIn) { note(e.message ?: "The sign-in failed", err = true); showAccount() } }
                    return@background
                }
                when (r.optString("state")) {
                    "slow_down" -> wait += 5
                    "done" -> {
                        runOnUiThread {
                            if (mine == signIn) {
                                toast("Signed in as ${r.optString("login")}")
                                loadAccount(fresh = true)
                            }
                        }
                        return@background
                    }
                }
            }
            runOnUiThread { if (mine == signIn) { note("The code expired: sign in again", err = true); showAccount() } }
        })
    }

    private fun showCode(c: JSONObject, mine: Int) {
        note("")
        val code = c.optString("user_code")
        val uri = c.optString("verification_uri", "https://github.com/login/device")
        account.removeAllViews()
        account.addView(Ui.text(this, "Enter this code on GitHub", 15.5f, bold = true))
        account.addView(Ui.text(this, code, 30f, Ui.ACCENT, bold = true).apply {
            typeface = android.graphics.Typeface.MONOSPACE
            letterSpacing = 0.08f
            setTextIsSelectable(true)
            setPadding(0, Ui.dp(context, 8), 0, Ui.dp(context, 8))
        })
        account.addView(Ui.text(this, "Copy it, open GitHub, paste it and approve rCAD. This page carries on by itself.", 12.5f, Ui.MUTED)
            .apply { setPadding(0, 0, 0, Ui.dp(context, 10)) })
        account.addView(Ui.row(this,
            Ui.accentButton(this, "Copy code and open GitHub") {
                (getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("GitHub code", code))
                try {
                    startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri)))
                } catch (e: android.content.ActivityNotFoundException) {
                    toast("No browser to open $uri")
                }
            },
            View(this).apply { layoutParams = LinearLayout.LayoutParams(Ui.dp(context, 8), 1) },
            Ui.outlineButton(this, "Cancel") { if (mine == signIn) { signIn++; showAccount() } },
        ))
        account.addView(Ui.text(this, "Waiting for GitHub…", 12.5f, Ui.FAINT).apply { setPadding(0, Ui.dp(context, 10), 0, 0) })
    }

    private fun signOut() {
        background({ api ->
            api.post("/api/github/logout", JSONObject())
            runOnUiThread { toast("Signed out on netdash"); loadAccount(fresh = true) }
        })
    }

    // ---------------------------------------------------------------- repositories

    private fun loadRepos() {
        header.text = "Loading…"
        background({ api ->
            val res = api.get("/api/github/repos", readMs = 100000)
            val arr = res.optJSONArray("repos")
            val out = (0 until (arr?.length() ?: 0)).map { arr!!.getJSONObject(it) }
            runOnUiThread {
                repos = out
                header.text = "${out.size}"
                show()
            }
        }, failed = { header.text = ""; note(it, err = true) })
    }

    private fun show() {
        list.removeAllViews()
        if (user?.optBoolean("signed_in") != true) {
            list.addView(Ui.text(this, "Sign in to see your repositories.", 14f, Ui.MUTED).apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 14), 0, 0) })
            return
        }
        val q = search.text.toString().trim().lowercase()
        val shown = repos.filter { q.isEmpty() || it.optString("full_name").lowercase().contains(q) || it.optString("description").lowercase().contains(q) }
        val rcad = shown.filter { it.optBoolean("rcad") || it.has("doc") }
        val rest = shown.filter { !(it.optBoolean("rcad") || it.has("doc")) }
        if (rcad.isNotEmpty()) list.addView(groupTitle("rCAD projects"))
        rcad.forEach { list.addView(repoRow(it)) }
        if (rest.isNotEmpty()) list.addView(groupTitle(if (rcad.isEmpty()) "Repositories" else "Other repositories"))
        rest.forEach { list.addView(repoRow(it)) }
        if (shown.isEmpty() && repos.isNotEmpty()) list.addView(Ui.text(this, "No repository matches.", 14f, Ui.MUTED).apply { setPadding(Ui.dp(context, 14), Ui.dp(context, 14), 0, 0) })
    }

    private fun groupTitle(t: String) = Ui.text(this, t.uppercase(), 11.5f, Ui.FAINT, bold = true).apply {
        letterSpacing = 0.06f
        setPadding(Ui.dp(context, 14), Ui.dp(context, 14), Ui.dp(context, 14), Ui.dp(context, 4))
    }

    private fun repoRow(r: JSONObject): View {
        val full = r.optString("full_name")
        val here = r.optString("doc").takeIf { r.has("doc") && it.isNotEmpty() }
        val sub = buildList {
            r.optString("description").takeIf { it.isNotEmpty() && it != "null" && it != "Made with rCAD" }?.let { add(it) }
            if (r.optBoolean("private")) add("private")
            r.optString("pushed").takeIf { it.length >= 10 }?.let { add("pushed ${it.substring(0, 10)}") }
            if (here != null) add("on netdash")
        }.joinToString(" · ")
        val texts = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(Ui.text(context, full, 15f, bold = true).apply { isSingleLine = true; ellipsize = android.text.TextUtils.TruncateAt.END })
            if (sub.isNotEmpty()) addView(Ui.text(context, sub, 12.5f, if (here != null) Ui.ACCENT else Ui.MUTED).apply { maxLines = 2; ellipsize = android.text.TextUtils.TruncateAt.END })
        }
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.pressable(null)
            setPadding(Ui.dp(context, 14), Ui.dp(context, 11), Ui.dp(context, 10), Ui.dp(context, 11))
            addView(android.widget.ImageView(context).apply { setImageDrawable(Icon(context, if (r.optBoolean("rcad")) "partStudio" else "branch", Ui.ACCENT, 22)) },
                LinearLayout.LayoutParams(Ui.dp(context, 22), Ui.dp(context, 22)).apply { marginEnd = Ui.dp(context, 12) })
            addView(texts, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(android.widget.ImageView(context).apply { setImageDrawable(Icon(context, "chevronRight", Ui.FAINT, 18)) })
            isClickable = true
            setOnClickListener {
                when {
                    here != null -> open(here, r.optString("name"), null)
                    r.optBoolean("rcad") -> openRepo(r.optString("clone_url"), r.optString("name"))
                    else -> AlertDialog.Builder(this@GitHubActivity)
                        .setTitle(full)
                        .setMessage("This doesn't look like an rCAD project (rCAD marks the ones it publishes). netdash will download it to look for an rCAD document at its top, and remove the copy again if there's none. An empty repository becomes a new project.")
                        .setPositiveButton("Open") { _, _ -> openRepo(r.optString("clone_url"), r.optString("name")) }
                        .setNegativeButton("Cancel", null)
                        .show()
                }
            }
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(row)
            addView(Ui.divider(context).apply { (layoutParams as LinearLayout.LayoutParams).marginStart = Ui.dp(context, 48) })
        }
    }

    /** Clone it on netdash (or bring netdash's copy up to date), then open its document. */
    private fun openRepo(url: String, name: String) {
        if (busy) return
        busy = true
        note("Getting $name onto netdash…")
        background({ api ->
            try {
                val res = api.post("/api/github/clone", JSONObject().put("url", url), readMs = 320000)
                runOnUiThread {
                    busy = false
                    note("")
                    val msg = when {
                        res.optBoolean("empty") -> "$name was empty: started a new document in it"
                        else -> "Opened $name · it syncs with GitHub before and after each chat"
                    }
                    open(res.getString("id"), name, msg)
                }
            } catch (e: IOException) {
                runOnUiThread { busy = false; note(e.message ?: "Could not open $name", err = true) }
            }
        }, failed = { busy = false; note(it, err = true) })
    }

    private fun open(id: String, name: String, msg: String?) {
        if (msg != null) toast(msg)
        startActivity(Intent(this, DocActivity::class.java).putExtra("doc", id).putExtra("name", name))
        if (msg != null) loadRepos()
    }

    private fun addressDialog() {
        val field = EditText(this).apply { hint = "https://github.com/owner/name"; setSingleLine() }
        AlertDialog.Builder(this)
            .setTitle("Open a repository")
            .setMessage("Any repository this account can open (or a public one) with an rCAD document at its top, or an empty one to start a project in.")
            .setView(Ui.column(this, 20, field))
            .setPositiveButton("Open") { _, _ ->
                val u = field.text.toString().trim().let { if (it.startsWith("http")) it else "https://github.com/$it" }
                openRepo(u, u.trimEnd('/').removeSuffix(".git").substringAfterLast('/'))
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
}
