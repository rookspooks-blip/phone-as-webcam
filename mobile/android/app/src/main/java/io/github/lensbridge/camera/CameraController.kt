package io.github.lensbridge.camera

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Rect
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.MeteringRectangle
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.util.Range
import android.view.Surface
import org.json.JSONArray
import org.json.JSONObject
import kotlin.math.abs
import kotlin.math.roundToInt

/**
 * Camera2 wrapper. Everything runs on [handler]; callers post to it.
 * Formats and frame rates are enumerated from the device, nothing is hard-coded.
 */
class CameraController(context: Context, private val listener: Listener) {
    interface Listener {
        fun onCameraInterrupted(reason: String)
        fun onCameraResumed()
    }

    data class Format(val w: Int, val h: Int, val minFps: Int, val maxFps: Int)

    private val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    private val thread = HandlerThread("camera").apply { start() }
    val handler = Handler(thread.looper)

    var cameraId: String
    var width = 1280
    var height = 720
    var fps = 30
    var zoom = 1f
    var torch = false
    var focusMode = "continuous"
    var ev = 0f
    private var afRegion: MeteringRectangle? = null
    private var aeRegion: MeteringRectangle? = null

    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var outputs: List<Surface> = emptyList()
    private var wantOpen = false
    private var interrupted = false

    init {
        cameraId = cameraIds().firstOrNull { facing(it) == CameraCharacteristics.LENS_FACING_BACK } ?: cameraIds().firstOrNull() ?: "0"
        snapFormat()
        manager.registerAvailabilityCallback(object : CameraManager.AvailabilityCallback() {
            override fun onCameraAvailable(id: String) {
                if (id == cameraId && wantOpen && device == null) reopen()
            }
        }, handler)
    }

    // ------------------------------------------------------------ characteristics

    fun cameraIds(): List<String> = try {
        manager.cameraIdList.toList()
    } catch (e: Exception) {
        emptyList()
    }

    private fun chars(id: String = cameraId): CameraCharacteristics = manager.getCameraCharacteristics(id)

    fun facing(id: String = cameraId): Int? = runCatching { chars(id).get(CameraCharacteristics.LENS_FACING) }.getOrNull()

    fun isFront(id: String = cameraId) = facing(id) == CameraCharacteristics.LENS_FACING_FRONT

    fun sensorOrientation(id: String = cameraId): Int = runCatching { chars(id).get(CameraCharacteristics.SENSOR_ORIENTATION) }.getOrNull() ?: 90

    fun timestampRealtime(id: String = cameraId): Boolean = runCatching {
        chars(id).get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE) == CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME
    }.getOrDefault(false)

    private fun fpsRanges(id: String = cameraId): Array<Range<Int>> =
        runCatching { chars(id).get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES) }.getOrNull() ?: arrayOf(Range(15, 30))

    fun formats(id: String = cameraId): List<Format> {
        val c = runCatching { chars(id) }.getOrNull() ?: return emptyList()
        val map = c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return emptyList()
        val ranges = fpsRanges(id)
        val aeMax = ranges.maxOf { it.upper }
        val aeMin = ranges.minOf { it.lower }.coerceAtLeast(1)
        return (map.getOutputSizes(SurfaceTexture::class.java) ?: emptyArray())
            .filter { it.width >= 320 && it.width.toLong() * it.height <= 3840L * 2160 }
            .map { s ->
                val dur = map.getOutputMinFrameDuration(SurfaceTexture::class.java, s)
                val maxFps = if (dur > 0) minOf(aeMax, (1_000_000_000.0 / dur).roundToInt()) else aeMax
                Format(s.width, s.height, aeMin, maxFps)
            }
            .distinctBy { it.w to it.h }
            .sortedWith(compareByDescending<Format> { it.w * it.h }.thenByDescending { it.maxFps })
    }

    private fun zoomRange(id: String = cameraId): Pair<Float, Float> {
        val c = chars(id)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            c.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)?.let { return it.lower to minOf(it.upper, 15f) }
        }
        return 1f to minOf(c.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f, 15f)
    }

    private fun cameraName(id: String): String {
        val c = chars(id)
        val focal = c.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)?.firstOrNull()
        val base = when (facing(id)) {
            CameraCharacteristics.LENS_FACING_FRONT -> "Фронтальная"
            CameraCharacteristics.LENS_FACING_EXTERNAL -> "Внешняя"
            else -> "Задняя"
        }
        return if (focal != null) "$base · f=%.1f мм (id $id)".format(focal) else "$base (id $id)"
    }

    fun capsJson(): JSONObject {
        val cams = JSONArray()
        for (id in cameraIds()) {
            val c = runCatching { chars(id) }.getOrNull() ?: continue
            val (zmin, zmax) = zoomRange(id)
            val evRange = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
            val evStep = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)?.toFloat() ?: 0f
            val fmts = JSONArray()
            formats(id).forEach { fmts.put(JSONObject().put("w", it.w).put("h", it.h).put("min_fps", it.minFps).put("max_fps", it.maxFps)) }
            cams.put(
                JSONObject()
                    .put("id", id)
                    .put("name", cameraName(id))
                    .put("position", if (isFront(id)) "front" else "back")
                    .put("formats", fmts)
                    .put("zoom_min", zmin.toDouble())
                    .put("zoom_max", zmax.toDouble())
                    .put("torch", c.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true)
                    .put("focus_poi", (c.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF) ?: 0) > 0)
                    .put("exposure_poi", (c.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE) ?: 0) > 0)
                    .put("ev_min", ((evRange?.lower ?: 0) * evStep).toDouble())
                    .put("ev_max", ((evRange?.upper ?: 0) * evStep).toDouble())
            )
        }
        return JSONObject().put("t", "caps").put("cameras", cams).put("audio", JSONObject().put("rate", 48000).put("channels", 1))
    }

    /** Snaps width/height/fps to a supported format of the current camera. */
    fun snapFormat() {
        val f = formats()
        if (f.isEmpty()) return
        val best = f.minByOrNull { abs(it.w * it.h - width * height) + if (it.w == width && it.h == height) 0 else 1 }!!
        width = best.w
        height = best.h
        fps = fps.coerceIn(best.minFps, best.maxFps)
    }

    fun stateJson(): JSONObject = JSONObject()
        .put("camera", cameraId).put("w", width).put("h", height).put("fps", fps)
        .put("zoom", zoom.toDouble()).put("torch", torch).put("focus_mode", focusMode).put("ev", ev.toDouble())

    // ------------------------------------------------------------ open / close

    fun open(targets: List<Surface>) {
        outputs = targets
        wantOpen = true
        reopen()
    }

    @SuppressLint("MissingPermission") // CAMERA is granted before the service starts
    private fun reopen() {
        if (!wantOpen || device != null) return
        try {
            manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(d: CameraDevice) {
                    if (!wantOpen) {
                        d.close()
                        return
                    }
                    device = d
                    createSession(d)
                }

                override fun onDisconnected(d: CameraDevice) = lost(d, "камера занята другим приложением")

                override fun onError(d: CameraDevice, error: Int) = lost(d, "ошибка камеры $error")
            }, handler)
        } catch (e: Exception) {
            lost(null, "не удалось открыть камеру: ${e.message}")
        }
    }

    private fun lost(d: CameraDevice?, reason: String) {
        d?.close()
        device = null
        session = null
        if (wantOpen) {
            if (!interrupted) {
                interrupted = true
                listener.onCameraInterrupted(reason)
            }
            handler.postDelayed({ reopen() }, 2000)
        }
    }

    @Suppress("DEPRECATION") // the List<Surface> overload works on API 26+, SessionConfiguration needs 28
    private fun createSession(d: CameraDevice) {
        try {
            d.createCaptureSession(outputs, object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    session = s
                    updateRepeating()
                    if (interrupted) {
                        interrupted = false
                        listener.onCameraResumed()
                    }
                }

                override fun onConfigureFailed(s: CameraCaptureSession) = lost(d, "не удалось настроить камеру")
            }, handler)
        } catch (e: Exception) {
            lost(d, "сессия камеры: ${e.message}")
        }
    }

    fun close() {
        wantOpen = false
        runCatching { session?.close() }
        runCatching { device?.close() }
        session = null
        device = null
        interrupted = false
    }

    // ------------------------------------------------------------ requests

    private fun pickFpsRange(): Range<Int> {
        val r = fpsRanges()
        return r.firstOrNull { it.lower == fps && it.upper == fps }
            ?: r.filter { it.upper == fps }.maxByOrNull { it.lower }
            ?: r.filter { fps in it.lower..it.upper }.minByOrNull { it.upper - it.lower }
            ?: r.maxByOrNull { it.upper }!!
    }

    private fun builder(): CaptureRequest.Builder? {
        val d = device ?: return null
        val c = chars()
        val b = d.createCaptureRequest(CameraDevice.TEMPLATE_RECORD)
        outputs.forEach { b.addTarget(it) }
        b.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
        b.set(CaptureRequest.CONTROL_AE_MODE, CaptureRequest.CONTROL_AE_MODE_ON)
        b.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, pickFpsRange())
        b.set(CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE, CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF)
        val afModes = c.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        when {
            focusMode == "continuous" && CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO in afModes ->
                b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
            CaptureRequest.CONTROL_AF_MODE_AUTO in afModes -> b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_AUTO)
        }
        if ((c.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AF) ?: 0) > 0) afRegion?.let { b.set(CaptureRequest.CONTROL_AF_REGIONS, arrayOf(it)) }
        if ((c.get(CameraCharacteristics.CONTROL_MAX_REGIONS_AE) ?: 0) > 0) aeRegion?.let { b.set(CaptureRequest.CONTROL_AE_REGIONS, arrayOf(it)) }
        val (zmin, zmax) = zoomRange()
        zoom = zoom.coerceIn(zmin, zmax)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            b.set(CaptureRequest.CONTROL_ZOOM_RATIO, zoom)
        } else {
            c.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE)?.let { a ->
                val w = (a.width() / zoom).toInt()
                val h = (a.height() / zoom).toInt()
                val l = a.left + (a.width() - w) / 2
                val t = a.top + (a.height() - h) / 2
                b.set(CaptureRequest.SCALER_CROP_REGION, Rect(l, t, l + w, t + h))
            }
        }
        if (c.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true) {
            b.set(CaptureRequest.FLASH_MODE, if (torch) CaptureRequest.FLASH_MODE_TORCH else CaptureRequest.FLASH_MODE_OFF)
        } else {
            torch = false
        }
        val step = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)?.toFloat() ?: 0f
        val range = c.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
        if (step > 0f && range != null) {
            val idx = (ev / step).roundToInt().coerceIn(range.lower, range.upper)
            ev = idx * step
            b.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, idx)
        }
        return b
    }

    fun updateRepeating() {
        val s = session ?: return
        val b = builder() ?: return
        runCatching { s.setRepeatingRequest(b.build(), null, handler) }
    }

    /**
     * Focus/exposure at a point given in the streamed frame (0..1, top-left
     * origin). [rotation]/[flipH] describe how the frame was produced from the
     * upright image, so the point can be mapped back to sensor coordinates.
     */
    fun focusAt(x: Float, y: Float, focus: Boolean, exposure: Boolean, rotation: Int, flipH: Boolean) {
        val c = chars()
        val active = c.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE) ?: return
        var p = (if (flipH) 1f - x else x) to y
        p = rotCw(p, (360 - rotation) % 360) // frame → upright image
        if (isFront()) p = (1f - p.first) to p.second
        p = rotCw(p, (360 - sensorOrientation()) % 360) // upright → sensor
        val half = (minOf(active.width(), active.height()) * 0.08f).toInt()
        val cx = active.left + (p.first * active.width()).toInt()
        val cy = active.top + (p.second * active.height()).toInt()
        val rect = MeteringRectangle(
            maxOf(active.left, cx - half), maxOf(active.top, cy - half),
            half * 2, half * 2, MeteringRectangle.METERING_WEIGHT_MAX - 1,
        )
        if (focus) {
            afRegion = rect
            focusMode = "auto"
        }
        if (exposure) aeRegion = rect
        val s = session ?: return
        val b = builder() ?: return
        if (focus) {
            b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_CANCEL)
            runCatching { s.capture(b.build(), null, handler) }
            b.set(CaptureRequest.CONTROL_AF_TRIGGER, CaptureRequest.CONTROL_AF_TRIGGER_START)
            runCatching { s.capture(b.build(), null, handler) }
        }
        updateRepeating()
    }

    fun resetRegions() {
        afRegion = null
        aeRegion = null
    }

    fun release() {
        close()
        thread.quitSafely()
    }

    companion object {
        /** Image point after rotating the image clockwise by [deg]. */
        fun rotCw(p: Pair<Float, Float>, deg: Int): Pair<Float, Float> = when (deg) {
            90 -> (1f - p.second) to p.first
            180 -> (1f - p.first) to (1f - p.second)
            270 -> p.second to (1f - p.first)
            else -> p
        }
    }
}
