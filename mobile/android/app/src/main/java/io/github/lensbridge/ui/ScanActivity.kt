package io.github.lensbridge.ui

import android.annotation.SuppressLint
import android.app.Activity
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.view.Gravity
import android.view.Surface
import android.view.TextureView
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.Toast
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import io.github.lensbridge.pairing.PairingStore
import io.github.lensbridge.protocol.PairingUri

/** Scans the pairing QR code shown by the desktop app (Camera2 + ZXing, no Google services). */
class ScanActivity : Activity() {
    private val thread = HandlerThread("scan").apply { start() }
    private val handler = Handler(thread.looper)
    private var device: CameraDevice? = null
    private var reader: ImageReader? = null
    private lateinit var texture: TextureView
    @Volatile private var done = false
    private val zxing = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE), DecodeHintType.TRY_HARDER to true))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        texture = TextureView(this)
        val hint = TextView(this).apply {
            text = "Наведите камеру на QR-код в приложении Lensbridge на ПК"
            setBackgroundColor(0x99000000.toInt())
            setTextColor(0xFFFFFFFF.toInt())
            setPadding(32, 24, 32, 24)
        }
        setContentView(FrameLayout(this).apply {
            addView(texture)
            addView(hint, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM))
        })
        texture.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) = openCamera(st)
            override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) {}
            override fun onSurfaceTextureDestroyed(st: SurfaceTexture) = true
            override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
        }
    }

    @SuppressLint("MissingPermission") // granted in MainActivity before reaching this screen
    private fun openCamera(st: SurfaceTexture) {
        val manager = getSystemService(CAMERA_SERVICE) as CameraManager
        val id = manager.cameraIdList.firstOrNull {
            manager.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK
        } ?: manager.cameraIdList.firstOrNull() ?: return
        st.setDefaultBufferSize(1280, 720)
        val r = ImageReader.newInstance(1280, 720, ImageFormat.YUV_420_888, 2)
        r.setOnImageAvailableListener({ rd ->
            val img = rd.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                if (!done) decode(img.planes[0].buffer.let { b -> ByteArray(b.remaining()).also { b.get(it) } }, img.planes[0].rowStride, img.width, img.height)
            } finally {
                img.close()
            }
        }, handler)
        reader = r
        try {
            manager.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(d: CameraDevice) {
                    device = d
                    val preview = Surface(st)
                    @Suppress("DEPRECATION")
                    d.createCaptureSession(listOf(preview, r.surface), object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            val b = d.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW)
                            b.addTarget(preview)
                            b.addTarget(r.surface)
                            b.set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                            runCatching { s.setRepeatingRequest(b.build(), null, handler) }
                        }

                        override fun onConfigureFailed(s: CameraCaptureSession) = fail("Камера недоступна")
                    }, handler)
                }

                override fun onDisconnected(d: CameraDevice) {
                    d.close()
                    fail("Камера занята (идёт трансляция?)")
                }

                override fun onError(d: CameraDevice, error: Int) {
                    d.close()
                    fail("Ошибка камеры $error")
                }
            }, handler)
        } catch (e: Exception) {
            fail("Камера недоступна: ${e.message}")
        }
    }

    private fun decode(luma: ByteArray, rowStride: Int, w: Int, h: Int) {
        val src = PlanarYUVLuminanceSource(luma, rowStride, h, 0, 0, w, h, false)
        val text = try {
            zxing.decodeWithState(BinaryBitmap(HybridBinarizer(src))).text
        } catch (e: NotFoundException) {
            return
        } finally {
            zxing.reset()
        }
        val uri = PairingUri.parse(text) ?: return
        done = true
        PairingStore.get(this).add(uri.key, uri.pcName)
        runOnUiThread {
            Toast.makeText(this, "Сопряжено с «${uri.pcName}»", Toast.LENGTH_LONG).show()
            finish()
        }
    }

    private fun fail(msg: String) = runOnUiThread {
        Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
        finish()
    }

    override fun onDestroy() {
        runCatching { device?.close() }
        reader?.close()
        thread.quitSafely()
        super.onDestroy()
    }
}
