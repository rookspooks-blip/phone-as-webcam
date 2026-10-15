package io.github.lensbridge.camera

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageFormat
import android.graphics.Matrix
import android.graphics.Rect
import android.graphics.YuvImage
import android.media.Image
import java.io.ByteArrayOutputStream

/** YUV_420_888 camera image → JPEG (for the optional browser MJPEG stream). */
object YuvJpeg {
    fun encode(image: Image, rotation: Int, quality: Int): ByteArray {
        val nv21 = toNv21(image)
        val out = ByteArrayOutputStream()
        YuvImage(nv21, ImageFormat.NV21, image.width, image.height, null).compressToJpeg(Rect(0, 0, image.width, image.height), quality, out)
        if (rotation % 360 == 0) return out.toByteArray()
        val bmp = BitmapFactory.decodeByteArray(out.toByteArray(), 0, out.size())
        val rotated = Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, Matrix().apply { postRotate(rotation.toFloat()) }, true)
        val out2 = ByteArrayOutputStream()
        rotated.compress(Bitmap.CompressFormat.JPEG, quality, out2)
        bmp.recycle()
        rotated.recycle()
        return out2.toByteArray()
    }

    /** Handles arbitrary row/pixel strides of the U and V planes. */
    fun toNv21(image: Image): ByteArray {
        val w = image.width
        val h = image.height
        val out = ByteArray(w * h * 3 / 2)
        val y = image.planes[0]
        val yBuf = y.buffer
        var pos = 0
        for (row in 0 until h) {
            yBuf.position(row * y.rowStride)
            yBuf.get(out, pos, w)
            pos += w
        }
        val u = image.planes[1]
        val v = image.planes[2]
        val uBuf = u.buffer
        val vBuf = v.buffer
        for (row in 0 until h / 2) {
            for (col in 0 until w / 2) {
                val vi = row * v.rowStride + col * v.pixelStride
                val ui = row * u.rowStride + col * u.pixelStride
                out[pos++] = vBuf.get(vi)
                out[pos++] = uBuf.get(ui)
            }
        }
        return out
    }
}
