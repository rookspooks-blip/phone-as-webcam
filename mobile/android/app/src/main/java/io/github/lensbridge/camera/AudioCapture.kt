package io.github.lensbridge.camera

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.MediaRecorder
import kotlin.concurrent.thread

/** Microphone → PCM s16le 48 kHz mono in 10 ms chunks, timestamped in the server clock. */
class AudioCapture(private val onPcm: (pcm: ByteArray, ptsUs: Long) -> Unit) {
    @Volatile private var running = false
    private var worker: Thread? = null

    @SuppressLint("MissingPermission") // checked by the caller (StreamService)
    fun start(): Boolean {
        if (running) return true
        val minBuf = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) return false
        val rec = try {
            AudioRecord(MediaRecorder.AudioSource.CAMCORDER, RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, maxOf(minBuf, CHUNK_BYTES * 8))
        } catch (e: Exception) {
            return false
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            return false
        }
        running = true
        worker = thread(name = "audio", isDaemon = true) {
            val ts = AudioTimestamp()
            var framesRead = 0L
            try {
                rec.startRecording()
                while (running) {
                    val buf = ByteArray(CHUNK_BYTES)
                    var off = 0
                    while (off < buf.size && running) {
                        val n = rec.read(buf, off, buf.size - off)
                        if (n <= 0) break
                        off += n
                    }
                    if (off <= 0) continue
                    val pts = if (rec.getTimestamp(ts, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                        ts.nanoTime + (framesRead - ts.framePosition) * 1_000_000_000L / RATE
                    } else {
                        System.nanoTime() - off / 2 * 1_000_000_000L / RATE
                    }
                    framesRead += off / 2
                    onPcm(if (off == buf.size) buf else buf.copyOf(off), pts / 1000)
                }
            } finally {
                runCatching { rec.stop() }
                rec.release()
            }
        }
        return true
    }

    fun stop() {
        running = false
        worker?.join(500)
        worker = null
    }

    companion object {
        const val RATE = 48_000
        private const val CHUNK_BYTES = RATE / 100 * 2 // 10 ms
    }
}
