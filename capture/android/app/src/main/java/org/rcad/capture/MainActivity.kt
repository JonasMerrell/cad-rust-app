package org.rcad.capture

import android.Manifest
import android.annotation.SuppressLint
import android.app.AlertDialog
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.SensorManager
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.CaptureResult
import android.hardware.camera2.TotalCaptureResult
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.camera2.interop.Camera2CameraControl
import androidx.camera.camera2.interop.Camera2CameraInfo
import androidx.camera.camera2.interop.Camera2Interop
import androidx.camera.camera2.interop.CaptureRequestOptions
import androidx.camera.camera2.interop.ExperimentalCamera2Interop
import androidx.camera.core.Camera
import androidx.camera.core.CameraSelector
import androidx.camera.core.FocusMeteringAction
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.AspectRatioStrategy
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale

@androidx.annotation.OptIn(markerClass = [ExperimentalCamera2Interop::class])
class MainActivity : ComponentActivity() {

    private lateinit var prefs: Prefs
    private lateinit var previewView: PreviewView
    private lateinit var statusText: TextView
    private lateinit var sessionText: TextView
    private lateinit var hintText: TextView
    private lateinit var shutter: Button
    private lateinit var modeBtn: Button
    private lateinit var lockBtn: Button
    private lateinit var calBtn: Button
    private lateinit var flash: View

    private var camera: Camera? = null
    private var imageCapture: ImageCapture? = null
    private var session: LocalSession? = null
    private var inFlight = 0
    private var bursting = false
    private var autoRunning = false
    private var locked = false
    private var calMode = false
    private val main = Handler(Looper.getMainLooper())
    private lateinit var motion: MotionTrigger

    // Latest values from the preview stream, used to lock the camera.
    @Volatile private var curFocus: Float? = null
    @Volatile private var curIso: Int? = null
    @Volatile private var curExposure: Long? = null
    @Volatile private var curFocal: Float? = null

    private val askCamera = registerForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        if (ok) startCamera() else statusText.text = "Camera permission is needed to take photos"
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        prefs = Prefs(this)
        previewView = findViewById(R.id.preview)
        statusText = findViewById(R.id.status)
        sessionText = findViewById(R.id.sessionText)
        hintText = findViewById(R.id.hint)
        shutter = findViewById(R.id.shutter)
        modeBtn = findViewById(R.id.modeBtn)
        lockBtn = findViewById(R.id.lockBtn)
        calBtn = findViewById(R.id.calBtn)
        flash = findViewById(R.id.flash)
        previewView.scaleType = PreviewView.ScaleType.FIT_CENTER

        motion = MotionTrigger(getSystemService(SENSOR_SERVICE) as SensorManager, { capture() }, { hintText.text = it })
        motion.busy = { inFlight > 0 }

        Store.cleanStale(this)
        session = Store.all(this).lastOrNull { !it.finished }

        setupControls()
        handlePairing(intent)
        refreshUi()

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) startCamera()
        else askCamera.launch(Manifest.permission.CAMERA)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handlePairing(intent)
    }

    override fun onResume() {
        super.onResume()
        Uploader.listener = { refreshUi() }
        Uploader.kick(this)
    }

    override fun onPause() {
        super.onPause()
        stopAuto()
        bursting = false
        Uploader.listener = null
        if (Store.pendingCount(this) > 0 || Store.all(this).any { it.finished }) Uploader.scheduleRetry(this)
    }

    // ---------------------------------------------------------------- pairing

    private fun handlePairing(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme != "rcadcapture" || uri.host != "pair") return
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
        Uploader.kick(this)
        refreshUi()
    }

    // ---------------------------------------------------------------- camera

    private fun startCamera() {
        val future = ProcessCameraProvider.getInstance(this)
        future.addListener({
            val provider = future.get()
            val rs = ResolutionSelector.Builder()
                .setAspectRatioStrategy(AspectRatioStrategy.RATIO_4_3_FALLBACK_AUTO_STRATEGY)
                .setResolutionStrategy(ResolutionStrategy.HIGHEST_AVAILABLE_STRATEGY)
                .build()
            val pb = Preview.Builder().setResolutionSelector(
                ResolutionSelector.Builder().setAspectRatioStrategy(AspectRatioStrategy.RATIO_4_3_FALLBACK_AUTO_STRATEGY).build()
            )
            Camera2Interop.Extender(pb).setSessionCaptureCallback(object : CameraCaptureSession.CaptureCallback() {
                override fun onCaptureCompleted(s: CameraCaptureSession, r: CaptureRequest, result: TotalCaptureResult) {
                    result.get(CaptureResult.LENS_FOCUS_DISTANCE)?.let { curFocus = it }
                    result.get(CaptureResult.SENSOR_SENSITIVITY)?.let { curIso = it }
                    result.get(CaptureResult.SENSOR_EXPOSURE_TIME)?.let { curExposure = it }
                    result.get(CaptureResult.LENS_FOCAL_LENGTH)?.let { curFocal = it }
                }
            })
            val preview = pb.build()
            preview.setSurfaceProvider(previewView.surfaceProvider)
            val ic = ImageCapture.Builder()
                .setCaptureMode(ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY)
                .setFlashMode(ImageCapture.FLASH_MODE_OFF)
                .setJpegQuality(95)
                .setResolutionSelector(rs)
                .build()
            try {
                provider.unbindAll()
                camera = provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, preview, ic)
                imageCapture = ic
                locked = false
                refreshUi()
            } catch (e: Exception) {
                statusText.text = "Camera failed: ${e.message}"
            }
        }, ContextCompat.getMainExecutor(this))
    }

    private fun manualFocusSupported(): Boolean {
        val cam = camera ?: return false
        val info = Camera2CameraInfo.from(cam.cameraInfo)
        val minFocus = info.getCameraCharacteristic(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE) ?: 0f
        val caps = info.getCameraCharacteristic(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES) ?: IntArray(0)
        return minFocus > 0f && caps.contains(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR)
    }

    /** Freeze exposure, white balance and focus so every photo of the object matches. */
    private fun lock() {
        val cam = camera ?: return
        val b = CaptureRequestOptions.Builder()
            .setCaptureRequestOption(CaptureRequest.CONTROL_AE_LOCK, true)
            .setCaptureRequestOption(CaptureRequest.CONTROL_AWB_LOCK, true)
        val focus = curFocus
        val manual = manualFocusSupported() && focus != null
        if (manual) {
            b.setCaptureRequestOption(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_OFF)
            b.setCaptureRequestOption(CaptureRequest.LENS_FOCUS_DISTANCE, focus!!)
        } else {
            // No manual focus: focus once on the centre and keep it there.
            val p = previewView.meteringPointFactory.createPoint(previewView.width / 2f, previewView.height / 2f)
            cam.cameraControl.startFocusAndMetering(FocusMeteringAction.Builder(p, FocusMeteringAction.FLAG_AF).disableAutoCancel().build())
        }
        Camera2CameraControl.from(cam.cameraControl).setCaptureRequestOptions(b.build())
        locked = true
        session?.let { s ->
            val cam2 = JSONObject()
                .put("locked", true)
                .put("focus_locked", if (manual) "manual" else "af-hold")
                .put("focus_distance_diopters", curFocus ?: JSONObject.NULL)
                .put("iso", curIso ?: JSONObject.NULL)
                .put("exposure_ns", curExposure ?: JSONObject.NULL)
                .put("focal_length_mm", curFocal?.toDouble() ?: JSONObject.NULL)
            s.meta.put("camera", cam2)
            s.save()
        }
        refreshUi()
    }

    private fun unlock() {
        camera?.let {
            Camera2CameraControl.from(it.cameraControl).clearCaptureRequestOptions()
            it.cameraControl.cancelFocusAndMetering()
        }
        locked = false
        refreshUi()
    }

    // ---------------------------------------------------------------- capture

    private fun capture() {
        val s = session
        if (s == null) {
            stopAuto()
            bursting = false
            newSessionDialog()
            return
        }
        val ic = imageCapture ?: return
        if (inFlight >= 2) return
        if (prefs.autoLock && !locked) lock()
        val (part, final) = s.nextFile(calMode)
        inFlight++
        ic.takePicture(ImageCapture.OutputFileOptions.Builder(part).build(), ContextCompat.getMainExecutor(this),
            object : ImageCapture.OnImageSavedCallback {
                override fun onImageSaved(output: ImageCapture.OutputFileResults) {
                    inFlight--
                    if (!part.renameTo(final)) part.delete()
                    blink()
                    motion.shotTaken()
                    Uploader.kick(this@MainActivity)
                    refreshUi()
                    if (bursting) capture()
                }

                override fun onError(e: ImageCaptureException) {
                    inFlight--
                    part.delete()
                    toast("Photo failed: ${e.message}")
                    refreshUi()
                }
            })
        if (bursting && inFlight < 2) main.postDelayed({ if (bursting) capture() }, 120)
    }

    private fun blink() {
        flash.animate().cancel()
        flash.alpha = 0.6f
        flash.animate().alpha(0f).setDuration(150).start()
    }

    private val intervalTick = object : Runnable {
        override fun run() {
            if (!autoRunning) return
            if (inFlight == 0) capture()
            main.postDelayed(this, (prefs.intervalSec * 1000).toLong().coerceAtLeast(200))
        }
    }

    private fun startAuto() {
        if (session == null) {
            newSessionDialog()
            return
        }
        autoRunning = true
        when (prefs.mode) {
            1 -> main.post(intervalTick)
            2 -> {
                motion.stepDeg = prefs.stepDeg
                motion.start()
            }
        }
        refreshUi()
    }

    private fun stopAuto() {
        if (!autoRunning) return
        autoRunning = false
        main.removeCallbacks(intervalTick)
        motion.stop()
        refreshUi()
    }

    // ---------------------------------------------------------------- controls

    @SuppressLint("ClickableViewAccessibility")
    private fun setupControls() {
        shutter.setOnTouchListener { v, ev ->
            when (ev.action) {
                MotionEvent.ACTION_DOWN -> {
                    v.isPressed = true
                    if (prefs.mode == 0) {
                        capture()
                        main.postDelayed(burstStart, 400)
                    }
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    v.isPressed = false
                    main.removeCallbacks(burstStart)
                    bursting = false
                    if (prefs.mode != 0 && ev.action == MotionEvent.ACTION_UP) {
                        if (autoRunning) stopAuto() else startAuto()
                    }
                    refreshUi()
                }
            }
            true
        }

        modeBtn.setOnClickListener {
            stopAuto()
            prefs.mode = (prefs.mode + 1) % 3
            refreshUi()
        }

        lockBtn.setOnClickListener { if (locked) unlock() else lock() }

        previewView.setOnTouchListener { _, ev ->
            if (ev.action == MotionEvent.ACTION_UP) {
                if (locked) toast("Exposure and focus are locked. Tap Unlock to refocus.")
                else camera?.let { cam ->
                    val p = previewView.meteringPointFactory.createPoint(ev.x, ev.y)
                    cam.cameraControl.startFocusAndMetering(FocusMeteringAction.Builder(p).disableAutoCancel().build())
                    hintText.text = "Focused there. Lock when it looks right."
                }
            }
            true
        }

        calBtn.setOnClickListener {
            val s = session
            when {
                s == null -> newSessionDialog { calBtn.performClick() }
                calMode -> { calMode = false; refreshUi() }
                s.reference == null -> referenceDialog(s) { placementDialog(s) { calMode = true; refreshUi() } }
                else -> { calMode = true; refreshUi() }
            }
        }
        calBtn.setOnLongClickListener {
            session?.let { s -> referenceDialog(s) { placementDialog(s) { calMode = true; refreshUi() } } }
            true
        }

        findViewById<Button>(R.id.finishBtn).setOnClickListener { finishDialog() }
        findViewById<Button>(R.id.settingsBtn).setOnClickListener { settingsDialog() }
        findViewById<Button>(R.id.sessionsBtn).setOnClickListener { queueDialog() }
    }

    private val burstStart = Runnable {
        if (prefs.mode == 0 && session != null) {
            bursting = true
            refreshUi()
            capture()
        }
    }

    private fun refreshUi() {
        val st = Uploader.status
        val pending = Store.pendingCount(this)
        statusText.text = when {
            !prefs.paired -> "Not paired: open Settings, or scan the netdash pairing QR"
            st.connected -> "● ${st.host}  ·  $pending waiting"
            else -> "○ ${st.error ?: "connecting…"}  ·  $pending on phone"
        }
        statusText.setTextColor(if (st.connected) 0xFF8BE28B.toInt() else 0xFFFFB0A0.toInt())

        val s = session
        sessionText.text = if (s == null) "No session. Press the shutter to start one."
        else {
            val cal = if (s.calibrationShots > 0) " (${s.calibrationShots} calibration)" else ""
            "${s.name}  ·  ${s.shots} photos$cal  ·  ${s.uploaded} on netdash"
        }
        modeBtn.text = when (prefs.mode) {
            1 -> String.format(Locale.US, "Every %.1f s", prefs.intervalSec)
            2 -> String.format(Locale.US, "Walk-around %.0f°", prefs.stepDeg)
            else -> "Tap / hold"
        }
        lockBtn.text = if (locked) "Locked 🔒" else "Lock AE/AF"
        lockBtn.isActivated = locked
        calBtn.isActivated = calMode
        calBtn.text = if (calMode) "Calibration ON" else "Calibration photos"
        shutter.isActivated = bursting || autoRunning
        if (!autoRunning) hintText.text = when {
            calMode -> "Keep the ${s?.reference?.optString("label") ?: "reference"} fully in view and sharp. Don't move it: it must stay put for the whole session."
            prefs.mode == 0 -> "Tap for one photo, hold for a burst. Tap the preview to focus."
            prefs.mode == 1 -> "Tap the shutter to start or stop interval shooting."
            else -> "Tap the shutter, then walk slowly around the object. Photos fire as you turn."
        }
    }

    // ---------------------------------------------------------------- dialogs

    private fun field(hint: String, value: String = "", type: Int = InputType.TYPE_CLASS_TEXT) = EditText(this).apply {
        this.hint = hint
        setText(value)
        inputType = type
    }

    private fun column(vararg views: View) = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(48, 16, 48, 0)
        views.forEach { addView(it) }
    }

    private fun newSessionDialog(then: (() -> Unit)? = null) {
        val name = field("Object name, e.g. intake manifold")
        val notes = field("Notes (optional)")
        AlertDialog.Builder(this)
            .setTitle("New capture session")
            .setView(column(name, notes))
            .setPositiveButton("Start") { _, _ ->
                session = Store.create(this, name.text.toString().trim(), notes.text.toString().trim(), null)
                calMode = false
                if (locked) unlock()
                refreshUi()
                then?.invoke()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /** Known-size reference objects for calibration (scale) photos. */
    private data class RefPreset(val label: String, val kind: String, val dims: List<Double>)

    private val presets = listOf(
        RefPreset("1-2-3 block (25.4 × 50.8 × 76.2 mm)", "123-block", listOf(25.4, 50.8, 76.2)),
        RefPreset("2-4-6 block (50.8 × 101.6 × 152.4 mm)", "246-block", listOf(50.8, 101.6, 152.4)),
        RefPreset("Ruler (distance between two marks)", "ruler", emptyList()),
        RefPreset("Other box / gauge block (enter W × H × D)", "box", emptyList()),
        RefPreset("Ball / sphere (enter diameter)", "sphere", emptyList()),
    )

    private fun referenceDialog(s: LocalSession, then: () -> Unit) {
        AlertDialog.Builder(this)
            .setTitle("What known-size object is in the calibration photos?")
            .setItems(presets.map { it.label }.toTypedArray()) { _, i ->
                val p = presets[i]
                if (p.dims.isNotEmpty()) {
                    setReference(s, p.kind, p.label.substringBefore(" ("), p.dims)
                    then()
                } else sizeDialog(s, p, then)
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun sizeDialog(s: LocalSession, p: RefPreset, then: () -> Unit) {
        val num = InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL
        val fields = when (p.kind) {
            "ruler" -> listOf(field("Length between the marks you'll use, mm", "100", num))
            "sphere" -> listOf(field("Diameter, mm", "", num))
            else -> listOf(field("Width, mm", "", num), field("Height, mm", "", num), field("Depth, mm", "", num))
        }
        val label = field("Name (optional)", "")
        AlertDialog.Builder(this)
            .setTitle(p.label.substringBefore(" ("))
            .setView(column(*(fields + label).toTypedArray()))
            .setPositiveButton("Use") { _, _ ->
                val dims = fields.mapNotNull { it.text.toString().toDoubleOrNull() }.filter { it > 0 }
                if (dims.size != fields.size) {
                    toast("Enter every size in mm")
                    return@setPositiveButton
                }
                val name = label.text.toString().trim().ifEmpty {
                    when (p.kind) { "ruler" -> "ruler (${dims[0]} mm)"; "sphere" -> "ball Ø${dims[0]} mm"; else -> "box" }
                }
                setReference(s, p.kind, name, dims)
                if (p.kind == "ruler") { s.meta.put("ruler_mm", dims[0]); s.save() }
                then()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    /**
     * Photogrammetry needs a still scene: the reference only gives scale if it sits in
     * the same place, next to the part, in every photo of the session.
     */
    private fun placementDialog(s: LocalSession, then: () -> Unit) {
        val label = s.reference?.optString("label") ?: "reference"
        val late = s.shots > 0
        val msg = buildString {
            append("Put the $label next to the part (close, but not touching it) before the first photo, ")
            append("and leave it there without moving it until you finish the session.\n\n")
            append("Calibration photos are ordinary shots of the same scene where the $label is fully in view and sharp. ")
            append("Don't pick it up, turn it or move it between shots: rCAD then can't find it.")
            if (late) {
                append("\n\nThis session already has ${s.shots} photos. If the $label wasn't sitting in this spot for all of them, ")
                append("finish this session and start a new one with it in place from the start.")
            }
        }
        AlertDialog.Builder(this)
            .setTitle("Place the $label once, then leave it")
            .setMessage(msg)
            .setPositiveButton("It's in place") { _, _ -> then() }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun setReference(s: LocalSession, kind: String, label: String, dims: List<Double>) {
        s.reference = JSONObject().put("kind", kind).put("label", label).put("dims_mm", JSONArray(dims))
    }

    private fun finishDialog() {
        val s = session ?: return toast("No session running")
        stopAuto()
        AlertDialog.Builder(this)
            .setTitle("Finish “${s.name}”?")
            .setMessage("${s.shots} photos. Any still on the phone keep uploading to netdash, then they're deleted here.")
            .setPositiveButton("Finish") { _, _ ->
                s.finished = true
                session = null
                calMode = false
                unlock()
                Uploader.kick(this)
                refreshUi()
            }
            .setNegativeButton("Keep shooting", null)
            .show()
    }

    private fun queueDialog() {
        val list = Store.all(this)
        if (list.isEmpty()) {
            toast("Nothing waiting: every photo is on netdash")
            return
        }
        val rows = list.map { s ->
            val state = when {
                s === session || s.localId == session?.localId -> "shooting"
                s.finished -> "finishing"
                else -> "paused"
            }
            "${s.name}: ${s.pendingPhotos().size} waiting, ${s.uploaded}/${s.shots} uploaded ($state)"
        }
        AlertDialog.Builder(this)
            .setTitle("On this phone")
            .setItems(rows.toTypedArray()) { _, i -> sessionActions(list[i]) }
            .setPositiveButton("Upload now") { _, _ -> Uploader.kick(this) }
            .setNegativeButton("Close", null)
            .show()
    }

    private fun sessionActions(s: LocalSession) {
        val current = s.localId == session?.localId
        val opts = mutableListOf<String>()
        if (!current && !s.finished) opts += "Continue shooting this session"
        if (!s.finished) opts += "Finish (upload and close)"
        opts += "Delete its photos from the phone (not uploaded ones on netdash)"
        AlertDialog.Builder(this)
            .setTitle(s.name)
            .setItems(opts.toTypedArray()) { _, i ->
                when {
                    opts[i].startsWith("Continue") -> { session = s; calMode = false; unlock(); refreshUi() }
                    opts[i].startsWith("Finish") -> {
                        s.finished = true
                        if (current) { session = null; calMode = false }
                        Uploader.kick(this); refreshUi()
                    }
                    else -> AlertDialog.Builder(this)
                        .setMessage("Delete ${s.pendingPhotos().size} photos that haven't reached netdash yet?")
                        .setPositiveButton("Delete") { _, _ ->
                            if (current) session = null
                            s.dir.deleteRecursively()
                            refreshUi()
                        }
                        .setNegativeButton("Cancel", null)
                        .show()
                }
            }
            .show()
    }

    private fun settingsDialog() {
        val urls = field("Receiver address(es), comma separated", prefs.urls.joinToString(", "), InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI)
        val token = field("Token", prefs.token)
        val num = InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL
        val interval = field("Interval mode: seconds between photos", prefs.intervalSec.toString(), num)
        val step = field("Walk-around mode: degrees of turn per photo", prefs.stepDeg.toString(), num)
        val autoLock = CheckBox(this).apply {
            text = "Lock exposure, focus and white balance on the first photo"
            isChecked = prefs.autoLock
        }
        val help = TextView(this).apply {
            text = "Tip: scan the pairing QR code from netdash with the phone's camera to fill these in. " +
                "Example address: http://192.168.0.247:8765"
            setPadding(0, 16, 0, 0)
        }
        AlertDialog.Builder(this)
            .setTitle("Settings")
            .setView(column(urls, token, interval, step, autoLock, help))
            .setPositiveButton("Save") { _, _ ->
                prefs.urls = urls.text.toString().split(',', ' ', '\n').map { it.trim() }.filter { it.isNotEmpty() }
                    .map { if (it.startsWith("http")) it else "http://$it" }
                prefs.token = token.text.toString()
                interval.text.toString().toFloatOrNull()?.let { prefs.intervalSec = it.coerceIn(0.2f, 60f) }
                step.text.toString().toFloatOrNull()?.let { prefs.stepDeg = it.coerceIn(2f, 45f) }
                prefs.autoLock = autoLock.isChecked
                prefs.lastGoodUrl = ""
                Uploader.kick(this)
                refreshUi()
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun toast(msg: String) = Toast.makeText(this, msg, Toast.LENGTH_SHORT).show()
}
