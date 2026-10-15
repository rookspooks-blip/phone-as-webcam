package io.github.lensbridge.camera

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.view.Surface
import kotlin.concurrent.thread

/**
 * Hardware H.264 via MediaCodec with a Surface input. Baseline profile, no
 * B-frames, low-latency hints; output is Annex B and SPS/PPS are prepended
 * to every IDR.
 */
class H264Encoder(private val onFrame: (data: ByteArray, key: Boolean, ptsUs: Long) -> Unit) {
    private var codec: MediaCodec? = null
    @Volatile private var running = false
    private var drain: Thread? = null
    private var config = ByteArray(0)
    var width = 0
        private set
    var height = 0
        private set
    @Volatile var bitrate = 6_000_000
        private set

    fun start(w: Int, h: Int, fps: Int, bps: Int): Surface {
        stop()
        width = w
        height = h
        bitrate = bps
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bps)
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
            setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
            setInteger(MediaFormat.KEY_LEVEL, MediaCodecInfo.CodecProfileLevel.AVCLevel41)
            setInteger(MediaFormat.KEY_PRIORITY, 0) // realtime
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            // Re-send the last frame if the camera stalls, so the PC keeps receiving data.
            setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 500_000)
        }
        val c = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        try {
            c.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            // Some encoders reject level/low-latency keys: retry with the essentials.
            c.reset()
            format.removeKey(MediaFormat.KEY_LEVEL)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) format.removeKey(MediaFormat.KEY_LOW_LATENCY)
            c.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
        val surface = c.createInputSurface()
        c.start()
        codec = c
        running = true
        drain = thread(name = "h264-drain", isDaemon = true) { drainLoop(c) }
        return surface
    }

    private fun drainLoop(c: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (running) {
            val idx = try {
                c.dequeueOutputBuffer(info, 10_000)
            } catch (e: IllegalStateException) {
                break
            }
            if (idx < 0) continue
            val buf = c.getOutputBuffer(idx)
            if (buf != null && info.size > 0) {
                val data = ByteArray(info.size)
                buf.position(info.offset)
                buf.get(data, 0, info.size)
                if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
                    config = data
                } else {
                    val key = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                    onFrame(if (key) config + data else data, key, info.presentationTimeUs)
                }
            }
            try {
                c.releaseOutputBuffer(idx, false)
            } catch (e: IllegalStateException) {
                break
            }
        }
    }

    fun requestKeyframe() {
        runCatching { codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) }) }
    }

    fun setBitrate(bps: Int) {
        bitrate = bps.coerceIn(200_000, 50_000_000)
        runCatching { codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bitrate) }) }
    }

    fun stop() {
        running = false
        drain?.join(500)
        drain = null
        codec?.let {
            runCatching { it.stop() }
            runCatching { it.release() }
        }
        codec = null
    }
}
