package io.github.lensbridge.service

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.media.ImageReader
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.view.OrientationEventListener
import android.view.Surface
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import io.github.lensbridge.R
import io.github.lensbridge.camera.AudioCapture
import io.github.lensbridge.camera.CameraController
import io.github.lensbridge.camera.GlRenderer
import io.github.lensbridge.camera.H264Encoder
import io.github.lensbridge.camera.YuvJpeg
import io.github.lensbridge.net.HttpServer
import io.github.lensbridge.net.LbpServer
import io.github.lensbridge.net.LbpSession
import io.github.lensbridge.net.NsdAdvertiser
import io.github.lensbridge.pairing.PairingStore
import io.github.lensbridge.protocol.Lbp
import io.github.lensbridge.protocol.serverNowUs
import io.github.lensbridge.ui.MainActivity
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.UUID

data class UiState(
    val running: Boolean = false,
    val status: String = "Запуск…",
    val connectedPc: String? = null,
    val streaming: Boolean = false,
    val fps: Int = 0,
    val kbps: Long = 0,
    val viewers: Int = 0,
    val dimRequested: Boolean = false,
    val events: List<String> = emptyList(),
)

/**
 * Foreground service (types camera|microphone) that owns the server, the
 * camera pipeline and the wake/Wi-Fi locks, so streaming continues with the
 * screen off.
 */
class StreamService : Service(), LbpServer.Listener, CameraController.Listener {
    companion object {
        private const val CHANNEL = "stream"
        private const val NOTIFICATION_ID = 1
        const val ACTION_STOP = "io.github.lensbridge.STOP"
        private val _state = MutableStateFlow(UiState())
        val state: StateFlow<UiState> = _state
        @Volatile var instance: StreamService? = null
            private set

        fun start(context: Context) {
            ContextCompat.startForegroundService(context, Intent(context, StreamService::class.java))
        }

        fun stop(context: Context) {
            context.startService(Intent(context, StreamService::class.java).setAction(ACTION_STOP))
        }

        fun clearDimRequest() = update { it.copy(dimRequested = false) }

        private fun update(f: (UiState) -> UiState) {
            synchronized(_state) { _state.value = f(_state.value) }
        }
    }

    private lateinit var prefs: android.content.SharedPreferences
    private lateinit var pairing: PairingStore
    private lateinit var server: LbpServer
    private lateinit var nsd: NsdAdvertiser
    private lateinit var camera: CameraController
    private var http: HttpServer? = null
    private val main = Handler(Looper.getMainLooper())
    private val jpegThread = HandlerThread("jpeg").apply { start() }
    private val jpegHandler = Handler(jpegThread.looper)

    private var gl: GlRenderer? = null
    private var cameraSurface: Surface? = null
    private var jpegReader: ImageReader? = null
    private val encoder = H264Encoder(::onEncoded)
    private val audio = AudioCapture(::onPcm)
    private var pipelineRunning = false
    private var rotation = 0
    private var flipH = false

    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var orientationListener: OrientationEventListener? = null
    @Volatile private var deviceOrientation = 0

    private val lock = Any()
    @Volatile private var session: LbpSession? = null
    private var videoSeq = 0
    private var audioSeq = 0
    private var jpegSeq = 0
    private var pendingDiscont = true
    private var framesThisSecond = 0
    private var dropped = 0
    private var jpegBusy = false
    private var frameCounter = 0
    @Volatile private var audioWanted = true
    @Volatile private var mirror = false
    @Volatile private var dim = false

    private val tick = object : Runnable {
        override fun run() {
            stats()
            main.postDelayed(this, 1000)
        }
    }

    // ------------------------------------------------------------ settings

    val orientationMode: String get() = prefs.getString("orientationMode", "auto")!!
    private val rotationOverride: Int get() = prefs.getInt("rotationOverride", -1)
    private val flipFront: Boolean get() = prefs.getBoolean("flipFront", true)
    private val httpEnabled: Boolean get() = prefs.getBoolean("httpEnabled", false)
    private val deviceName: String get() = prefs.getString("deviceName", null) ?: "${Build.MANUFACTURER} ${Build.MODEL}"
    private val installId: String
        get() = prefs.getString("installId", null) ?: UUID.randomUUID().toString().replace("-", "").take(16).also {
            prefs.edit().putString("installId", it).apply()
        }

    // ------------------------------------------------------------ lifecycle

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        prefs = getSharedPreferences("settings", MODE_PRIVATE)
        instance = this
        createChannel()
        var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
        if (granted(Manifest.permission.RECORD_AUDIO)) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification("Ожидание подключения ПК"), if (Build.VERSION.SDK_INT >= 30) types else 0)

        pairing = PairingStore.get(this)
        camera = CameraController(this, this)
        server = LbpServer(pairing::lookup, this)
        server.start()
        nsd = NsdAdvertiser(this)
        nsd.register(deviceName, installId) { note(it) }
        if (httpEnabled) startHttp()
        orientationListener = object : OrientationEventListener(this) {
            override fun onOrientationChanged(deg: Int) {
                if (deg == ORIENTATION_UNKNOWN) return
                val r = ((deg + 45) / 90 * 90) % 360
                if (r != deviceOrientation) {
                    deviceOrientation = r
                    if (orientationMode == "auto" && rotationOverride < 0) {
                        camera.handler.removeCallbacks(orientationRestart)
                        camera.handler.postDelayed(orientationRestart, 800)
                    }
                }
            }
        }.also { if (it.canDetectOrientation()) it.enable() }
        main.post(tick)
        update { it.copy(running = true) }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onDestroy() {
        instance = null
        main.removeCallbacks(tick)
        orientationListener?.disable()
        server.stop()
        nsd.unregister()
        http?.stop()
        camera.handler.post {
            stopPipeline()
            camera.release()
        }
        jpegThread.quitSafely()
        releaseLocks()
        update { UiState(running = false, status = "Сервер остановлен") }
        super.onDestroy()
    }

    /** Re-reads settings changed in the UI. */
    fun settingsChanged() {
        if (httpEnabled && http == null) startHttp()
        if (!httpEnabled && http != null) {
            http?.stop()
            http = null
        }
        camera.handler.post { if (pipelineRunning) restartPipeline() }
    }

    private val orientationRestart = Runnable { if (pipelineRunning && rotationFor() != rotation) restartPipeline() }

    private fun granted(p: String) = ContextCompat.checkSelfPermission(this, p) == PackageManager.PERMISSION_GRANTED

    private fun startHttp() {
        http = HttpServer({ deviceName }) { n ->
            update { it.copy(viewers = n) }
            camera.handler.post { updatePipelineNeeded() }
        }.also { it.start() }
    }

    private fun note(msg: String) {
        val t = SimpleDateFormat("HH:mm:ss", Locale.US).format(Date())
        update { it.copy(events = (it.events + "$t  $msg").takeLast(60)) }
    }

    private fun createChannel() {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Трансляция", NotificationManager.IMPORTANCE_LOW))
    }

    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 1, Intent(this, StreamService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE)
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_camera)
            .setContentTitle("Lensbridge")
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(0, "Остановить", stop)
            .build()
    }

    private fun setNotification(text: String) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(text))
    }

    private fun acquireLocks() {
        if (wakeLock == null) {
            wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lensbridge:stream").apply { acquire() }
        }
        if (wifiLock == null) {
            val mode = if (Build.VERSION.SDK_INT >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else @Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF
            wifiLock = (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager).createWifiLock(mode, "lensbridge:wifi").apply { acquire() }
        }
    }

    private fun releaseLocks() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        wifiLock?.let { if (it.isHeld) it.release() }
        wifiLock = null
    }

    // ------------------------------------------------------------ pipeline (camera.handler thread)

    /** Clockwise rotation applied to the upright image. */
    private fun rotationFor(): Int {
        rotationOverride.takeIf { it >= 0 }?.let { return it }
        return when (orientationMode) {
            "landscape" -> 90
            "portrait" -> 0
            else -> deviceOrientation
        }
    }

    private fun updatePipelineNeeded() {
        val needed = session != null || (http?.viewerCount ?: 0) > 0
        if (needed && !pipelineRunning) startPipeline()
        if (!needed && pipelineRunning) stopPipeline()
    }

    private fun startPipeline() {
        if (!granted(Manifest.permission.CAMERA)) {
            note("Нет разрешения на камеру")
            return
        }
        val so = camera.sensorOrientation()
        rotation = rotationFor()
        flipH = camera.isFront() && flipFront
        val (nw, nh) = if (so % 180 != 0) camera.height to camera.width else camera.width to camera.height
        val (ow, oh) = if (rotation % 180 != 0) nh to nw else nw to nh

        val renderer = GlRenderer(camera.timestampRealtime())
        val st: SurfaceTexture = renderer.start()
        st.setDefaultBufferSize(camera.width, camera.height)
        renderer.rotation = rotation
        renderer.flipH = flipH
        renderer.onFrame = { synchronized(lock) { framesThisSecond++ } }
        val encSurface = try {
            encoder.start(ow, oh, camera.fps, encoder.bitrate)
        } catch (e: Exception) {
            note("Кодер H.264 не запустился: ${e.message}")
            renderer.release()
            return
        }
        renderer.setEncoderSurface(encSurface, ow, oh)
        gl = renderer
        val surfaces = mutableListOf(Surface(st).also { cameraSurface = it })
        if (httpEnabled) jpegSurface()?.let { surfaces += it }
        camera.open(surfaces)
        if (audioWanted && granted(Manifest.permission.RECORD_AUDIO)) audio.start()
        synchronized(lock) { pendingDiscont = true }
        pipelineRunning = true
        update { it.copy(streaming = true) }
        session?.let { sendStreamConfig(it, ow, oh) }
    }

    private fun stopPipeline() {
        camera.close()
        audio.stop()
        gl?.setEncoderSurface(null, 0, 0)
        encoder.stop()
        gl?.release()
        gl = null
        cameraSurface?.release()
        cameraSurface = null
        jpegReader?.close()
        jpegReader = null
        pipelineRunning = false
        update { it.copy(streaming = false) }
    }

    private fun restartPipeline() {
        stopPipeline()
        startPipeline()
    }

    private fun jpegSurface(): Surface? {
        val map = (getSystemService(CAMERA_SERVICE) as CameraManager).getCameraCharacteristics(camera.cameraId)
            .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return null
        val aspect = camera.width.toFloat() / camera.height
        val size = map.getOutputSizes(ImageFormat.YUV_420_888)
            ?.filter { it.width <= 960 }
            ?.minByOrNull { kotlin.math.abs(it.width.toFloat() / it.height - aspect) * 1000 + kotlin.math.abs(it.width - 640) }
            ?: return null
        val reader = ImageReader.newInstance(size.width, size.height, ImageFormat.YUV_420_888, 2)
        reader.setOnImageAvailableListener({ r ->
            val img = r.acquireLatestImage() ?: return@setOnImageAvailableListener
            val want = synchronized(lock) {
                frameCounter++
                val w = (http?.viewerCount ?: 0) > 0 && frameCounter % 3 == 0 && !jpegBusy
                if (w) jpegBusy = true
                w
            }
            if (!want) {
                img.close()
                return@setOnImageAvailableListener
            }
            val pts = serverNowUs()
            val jpegRotation = if (camera.isFront()) (camera.sensorOrientation() - rotation + 360) % 360 else (camera.sensorOrientation() + rotation) % 360
            val jpeg = try {
                YuvJpeg.encode(img, jpegRotation, 70)
            } finally {
                img.close()
            }
            val seq = synchronized(lock) {
                jpegBusy = false
                jpegSeq++
            }
            http?.broadcast(jpeg, seq, pts)
        }, jpegHandler)
        jpegReader = reader
        return reader.surface
    }

    // ------------------------------------------------------------ media out

    private fun onEncoded(data: ByteArray, key: Boolean, ptsUs: Long) {
        val s = session ?: return
        var flags = if (key) Lbp.FLAG_KEY or Lbp.FLAG_CONFIG else 0
        val seq: Int
        synchronized(lock) {
            if (pendingDiscont) {
                if (!key) {
                    encoder.requestKeyframe()
                    return
                }
                flags = flags or Lbp.FLAG_DISCONT
                pendingDiscont = false
            }
            seq = videoSeq
        }
        // Allow ~150 ms of queued video, then drop until the next IDR.
        val limit = maxOf(256L * 1024, encoder.bitrate / 8L * 150 / 1000)
        if (s.sendMedia(Lbp.TYPE_VIDEO, Lbp.media(Lbp.CODEC_H264, flags, seq, ptsUs, data), true, limit)) {
            synchronized(lock) { videoSeq++ }
        } else {
            synchronized(lock) {
                dropped++
                pendingDiscont = true
            }
            encoder.requestKeyframe()
        }
    }

    private fun onPcm(pcm: ByteArray, ptsUs: Long) {
        val s = session ?: return
        if (!audioWanted) return
        val seq = synchronized(lock) { audioSeq++ }
        s.sendMedia(Lbp.TYPE_AUDIO, Lbp.media(Lbp.CODEC_PCM, 0, seq, ptsUs, pcm), true, 4L * 1024 * 1024)
    }

    private fun sendStreamConfig(s: LbpSession, w: Int = encoder.width, h: Int = encoder.height) {
        s.sendJson(
            JSONObject().put("t", "stream_config")
                .put("video", JSONObject().put("codec", "h264").put("w", w).put("h", h).put("fps", camera.fps))
                .put("audio", JSONObject().put("codec", "pcm_s16le").put("rate", AudioCapture.RATE).put("channels", 1))
        )
    }

    private fun fullState(): JSONObject = camera.stateJson()
        .put("bitrate", encoder.bitrate).put("audio", audioWanted).put("mirror", mirror).put("screen", if (dim) "dim" else "on")

    private fun stats() {
        val frames = synchronized(lock) { framesThisSecond.also { framesThisSecond = 0 } }
        val s = session
        val kbps = (s?.takeSentBytes() ?: 0) * 8 / 1000
        update { it.copy(fps = frames, kbps = kbps) }
        if (s == null) return
        val bm = getSystemService(BATTERY_SERVICE) as BatteryManager
        val thermal = if (Build.VERSION.SDK_INT >= 29) {
            when ((getSystemService(POWER_SERVICE) as PowerManager).currentThermalStatus) {
                PowerManager.THERMAL_STATUS_NONE, PowerManager.THERMAL_STATUS_LIGHT -> "nominal"
                PowerManager.THERMAL_STATUS_MODERATE -> "fair"
                PowerManager.THERMAL_STATUS_SEVERE -> "serious"
                else -> "critical"
            }
        } else "nominal"
        s.sendJson(
            JSONObject().put("t", "stats").put("fps", frames).put("kbps", kbps).put("dropped", synchronized(lock) { dropped })
                .put("battery", bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY) / 100.0)
                .put("charging", bm.isCharging).put("thermal", thermal)
        )
    }

    // ------------------------------------------------------------ server callbacks

    override fun onServerState(text: String) = update { it.copy(status = text) }

    override fun onSessionStarted(session: LbpSession, hello: JSONObject) {
        this.session = session
        synchronized(lock) {
            videoSeq = 0
            audioSeq = 0
            dropped = 0
            pendingDiscont = true
        }
        main.post {
            acquireLocks()
            setNotification("Передача на «${session.clientName}»")
            update { it.copy(connectedPc = session.clientName) }
            note("Подключён ПК «${session.clientName}» (${session.peer})")
        }
        camera.handler.post {
            updatePipelineNeeded()
            session.sendJson(
                JSONObject().put("t", "welcome").put("proto", Lbp.PROTO).put("server", "lensbridge-android")
                    .put("version", packageManager.getPackageInfo(packageName, 0).versionName)
                    .put("device", deviceName).put("server_time_us", serverNowUs()).put("state", fullState())
            )
            sendStreamConfig(session)
            encoder.requestKeyframe()
        }
    }

    override fun onSessionControl(session: LbpSession, msg: JSONObject) {
        when (msg.optString("t")) {
            "ping" -> session.sendJson(JSONObject().put("t", "pong").put("id", msg.opt("id")).put("c0", msg.opt("c0")).put("s1", serverNowUs()))
            "get_caps" -> camera.handler.post { session.sendJson(camera.capsJson().put("id", msg.opt("id"))) }
            "request_keyframe" -> encoder.requestKeyframe()
            "set" -> camera.handler.post { handleSet(session, msg) }
            "bye" -> session.close("ПК отключился")
            else -> session.sendJson(JSONObject().put("t", "error").put("id", msg.opt("id")).put("code", "bad_request"))
        }
    }

    /** Runs on camera.handler. */
    private fun handleSet(session: LbpSession, msg: JSONObject) {
        val p = msg.optJSONObject("p") ?: JSONObject()
        var restart = false
        if (p.has("camera")) {
            val id = p.getString("camera")
            if (id != camera.cameraId && id in camera.cameraIds()) {
                camera.cameraId = id
                camera.zoom = 1f
                camera.torch = false
                camera.resetRegions()
                restart = true
            }
        }
        if (p.has("w") && p.has("h")) {
            camera.width = p.getInt("w")
            camera.height = p.getInt("h")
            restart = true
        }
        if (p.has("fps")) {
            camera.fps = p.getInt("fps")
            restart = true
        }
        if (restart) camera.snapFormat()
        if (p.has("bitrate")) encoder.setBitrate(p.getInt("bitrate"))
        if (p.has("mirror")) mirror = p.getBoolean("mirror")
        if (p.has("screen")) {
            dim = p.getString("screen") == "dim"
            update { it.copy(dimRequested = dim) }
        }
        if (p.has("audio")) {
            audioWanted = p.getBoolean("audio")
            if (pipelineRunning) {
                if (audioWanted && granted(Manifest.permission.RECORD_AUDIO)) audio.start() else audio.stop()
            }
        }
        var controls = false
        if (p.has("zoom")) { camera.zoom = p.getDouble("zoom").toFloat(); controls = true }
        if (p.has("torch")) { camera.torch = p.getBoolean("torch"); controls = true }
        if (p.has("ev")) { camera.ev = p.getDouble("ev").toFloat(); controls = true }
        if (p.has("focus_mode")) {
            camera.focusMode = p.getString("focus_mode")
            if (camera.focusMode == "continuous") camera.resetRegions()
            controls = true
        }
        if (restart && pipelineRunning) {
            restartPipeline()
        } else {
            val f = p.optJSONObject("focus")
            val e = p.optJSONObject("exposure")
            if (f != null || e != null) {
                val pt = f ?: e!!
                camera.focusAt(pt.getDouble("x").toFloat(), pt.getDouble("y").toFloat(), f != null, e != null, rotation, flipH)
            } else if (controls) {
                camera.updateRepeating()
            }
        }
        session.sendJson(JSONObject().put("t", "ack").put("id", msg.opt("id")).put("applied", fullState()))
    }

    override fun onSessionEnded(session: LbpSession, reason: String) {
        if (this.session === session) this.session = null
        main.post {
            releaseLocks()
            setNotification("Ожидание подключения ПК")
            update { it.copy(connectedPc = null) }
            note("ПК отключён: $reason")
        }
        camera.handler.post { updatePipelineNeeded() }
    }

    // ------------------------------------------------------------ camera callbacks

    override fun onCameraInterrupted(reason: String) {
        session?.sendJson(JSONObject().put("t", "event").put("kind", "interrupted").put("detail", reason))
        main.post { note("Камера прервана: $reason") }
    }

    override fun onCameraResumed() {
        synchronized(lock) { pendingDiscont = true }
        encoder.requestKeyframe()
        session?.sendJson(JSONObject().put("t", "event").put("kind", "resumed").put("detail", ""))
        main.post { note("Камера снова работает") }
    }
}
