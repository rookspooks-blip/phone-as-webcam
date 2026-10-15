package io.github.lensbridge.camera

import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.util.concurrent.CountDownLatch

/**
 * Camera frames (SurfaceTexture, OES texture) → rotated/flipped → encoder input surface.
 *
 * SurfaceTexture's transform matrix makes the image upright for the device's
 * natural (portrait) orientation; [rotation] then rotates it clockwise by
 * 0/90/180/270 so it is upright for how the phone is actually held.
 */
class GlRenderer(private val timestampRealtime: Boolean) {
    private val thread = HandlerThread("gl").apply { start() }
    private val handler = Handler(thread.looper)

    private var display: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var context: EGLContext = EGL14.EGL_NO_CONTEXT
    private var config: EGLConfig? = null
    private var pbuffer: EGLSurface = EGL14.EGL_NO_SURFACE
    private var encoderSurface: EGLSurface = EGL14.EGL_NO_SURFACE
    private var outW = 0
    private var outH = 0
    private var program = 0
    private var texId = 0
    private lateinit var surfaceTexture: SurfaceTexture
    private val texMatrix = FloatArray(16)

    @Volatile var rotation = 0
    @Volatile var flipH = false
    /** Called on the GL thread after each frame is submitted to the encoder (pts in µs). */
    var onFrame: ((Long) -> Unit)? = null

    private val positions: FloatBuffer = floatBuffer(floatArrayOf(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f))
    private val texCoords: FloatBuffer = floatBuffer(FloatArray(8))

    /** Creates the EGL context and the camera SurfaceTexture (blocking). */
    fun start(): SurfaceTexture {
        val latch = CountDownLatch(1)
        handler.post {
            setupEgl()
            surfaceTexture = SurfaceTexture(texId)
            surfaceTexture.setOnFrameAvailableListener({ drawFrame() }, handler)
            latch.countDown()
        }
        latch.await()
        return surfaceTexture
    }

    fun setEncoderSurface(surface: Surface?, width: Int, height: Int) {
        val latch = CountDownLatch(1)
        handler.post {
            if (encoderSurface != EGL14.EGL_NO_SURFACE) {
                EGL14.eglMakeCurrent(display, pbuffer, pbuffer, context)
                EGL14.eglDestroySurface(display, encoderSurface)
                encoderSurface = EGL14.EGL_NO_SURFACE
            }
            if (surface != null) {
                encoderSurface = EGL14.eglCreateWindowSurface(display, config, surface, intArrayOf(EGL14.EGL_NONE), 0)
                outW = width
                outH = height
            }
            latch.countDown()
        }
        latch.await()
    }

    fun release() {
        handler.post {
            runCatching { surfaceTexture.release() }
            if (display != EGL14.EGL_NO_DISPLAY) {
                EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                if (encoderSurface != EGL14.EGL_NO_SURFACE) EGL14.eglDestroySurface(display, encoderSurface)
                EGL14.eglDestroySurface(display, pbuffer)
                EGL14.eglDestroyContext(display, context)
                EGL14.eglTerminate(display)
            }
            display = EGL14.EGL_NO_DISPLAY
            thread.quitSafely()
        }
    }

    private fun setupEgl() {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val version = IntArray(2)
        EGL14.eglInitialize(display, version, 0, version, 1)
        val attribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL_RECORDABLE_ANDROID, 1,
            EGL14.EGL_NONE,
        )
        val configs = arrayOfNulls<EGLConfig>(1)
        val num = IntArray(1)
        EGL14.eglChooseConfig(display, attribs, 0, configs, 0, 1, num, 0)
        config = configs[0]
        context = EGL14.eglCreateContext(display, config, EGL14.EGL_NO_CONTEXT, intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        pbuffer = EGL14.eglCreatePbufferSurface(display, config, intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0)
        EGL14.eglMakeCurrent(display, pbuffer, pbuffer, context)

        program = buildProgram(VERTEX, FRAGMENT)
        val tex = IntArray(1)
        GLES20.glGenTextures(1, tex, 0)
        texId = tex[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texId)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
    }

    /** Camera timestamp → server clock (System.nanoTime base), µs. */
    private fun ptsUs(cameraTsNs: Long): Long {
        val now = System.nanoTime()
        val ns = when {
            cameraTsNs <= 0 -> now
            timestampRealtime -> cameraTsNs - (SystemClock.elapsedRealtimeNanos() - now)
            kotlin.math.abs(cameraTsNs - now) < 1_000_000_000L -> cameraTsNs
            else -> now
        }
        return ns / 1000
    }

    private fun drawFrame() {
        if (display == EGL14.EGL_NO_DISPLAY) return
        val target = if (encoderSurface != EGL14.EGL_NO_SURFACE) encoderSurface else pbuffer
        EGL14.eglMakeCurrent(display, target, target, context)
        surfaceTexture.updateTexImage()
        if (target == pbuffer) return // nobody consumes frames: just keep the queue drained
        surfaceTexture.getTransformMatrix(texMatrix)
        val pts = ptsUs(surfaceTexture.timestamp)

        // Per-vertex texture coords: rotate clockwise by `rotation`, then mirror.
        val base = floatArrayOf(0f, 0f, 1f, 0f, 0f, 1f, 1f, 1f)
        val tc = FloatArray(8)
        for (i in 0 until 4) {
            val s = base[i * 2]
            val t = base[i * 2 + 1]
            var (x, y) = when (rotation) {
                90 -> (1f - t) to s
                180 -> (1f - s) to (1f - t)
                270 -> t to (1f - s)
                else -> s to t
            }
            if (flipH) x = 1f - x
            tc[i * 2] = x
            tc[i * 2 + 1] = y
        }
        texCoords.clear()
        texCoords.put(tc).position(0)

        GLES20.glViewport(0, 0, outW, outH)
        GLES20.glUseProgram(program)
        val aPos = GLES20.glGetAttribLocation(program, "aPos")
        val aTex = GLES20.glGetAttribLocation(program, "aTex")
        GLES20.glEnableVertexAttribArray(aPos)
        GLES20.glVertexAttribPointer(aPos, 2, GLES20.GL_FLOAT, false, 0, positions)
        GLES20.glEnableVertexAttribArray(aTex)
        GLES20.glVertexAttribPointer(aTex, 2, GLES20.GL_FLOAT, false, 0, texCoords)
        GLES20.glUniformMatrix4fv(GLES20.glGetUniformLocation(program, "uTexMatrix"), 1, false, texMatrix, 0)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texId)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
        EGLExt.eglPresentationTimeANDROID(display, encoderSurface, pts * 1000)
        EGL14.eglSwapBuffers(display, encoderSurface)
        onFrame?.invoke(pts)
    }

    private fun buildProgram(vs: String, fs: String): Int {
        fun compile(type: Int, src: String): Int {
            val sh = GLES20.glCreateShader(type)
            GLES20.glShaderSource(sh, src)
            GLES20.glCompileShader(sh)
            return sh
        }
        val p = GLES20.glCreateProgram()
        GLES20.glAttachShader(p, compile(GLES20.GL_VERTEX_SHADER, vs))
        GLES20.glAttachShader(p, compile(GLES20.GL_FRAGMENT_SHADER, fs))
        GLES20.glLinkProgram(p)
        return p
    }

    companion object {
        private const val EGL_RECORDABLE_ANDROID = 0x3142
        private const val VERTEX = """
            attribute vec4 aPos;
            attribute vec4 aTex;
            uniform mat4 uTexMatrix;
            varying vec2 vTex;
            void main() {
                gl_Position = aPos;
                vTex = (uTexMatrix * aTex).xy;
            }
        """
        private const val FRAGMENT = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            varying vec2 vTex;
            uniform samplerExternalOES sTex;
            void main() {
                gl_FragColor = texture2D(sTex, vTex);
            }
        """

        private fun floatBuffer(a: FloatArray): FloatBuffer =
            ByteBuffer.allocateDirect(a.size * 4).order(ByteOrder.nativeOrder()).asFloatBuffer().apply { put(a); position(0) }
    }
}
