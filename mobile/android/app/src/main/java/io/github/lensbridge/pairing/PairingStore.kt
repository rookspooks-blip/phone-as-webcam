package io.github.lensbridge.pairing

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import io.github.lensbridge.protocol.Lbp
import io.github.lensbridge.protocol.hex
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

data class PairedPc(val keyIdHex: String, val key: ByteArray, val name: String, val addedMs: Long)

/** Paired PCs' pre-shared keys, encrypted with an Android Keystore AES key. */
class PairingStore private constructor(context: Context) {
    private val prefs = context.getSharedPreferences("pairing", Context.MODE_PRIVATE)
    private val _pcs = MutableStateFlow<List<PairedPc>>(emptyList())
    val pcs: StateFlow<List<PairedPc>> = _pcs
    @Volatile private var byKeyId: Map<String, ByteArray> = emptyMap()

    init {
        load()
    }

    fun lookup(keyId: ByteArray): ByteArray? = byKeyId[keyId.hex()]

    @Synchronized
    fun add(key: ByteArray, name: String) {
        val id = Lbp.keyId(key).hex()
        save(_pcs.value.filter { it.keyIdHex != id } + PairedPc(id, key, name, System.currentTimeMillis()))
    }

    @Synchronized
    fun remove(keyIdHex: String) = save(_pcs.value.filter { it.keyIdHex != keyIdHex })

    private fun secretKey(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return gen.generateKey()
    }

    private fun load() {
        val list = try {
            val blob = prefs.getString("blob", null)?.let { Base64.decode(it, Base64.NO_WRAP) }
            if (blob == null || blob.size < 13) emptyList() else {
                val c = Cipher.getInstance("AES/GCM/NoPadding")
                c.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(128, blob, 0, 12))
                val json = JSONArray(String(c.doFinal(blob, 12, blob.size - 12), Charsets.UTF_8))
                (0 until json.length()).map {
                    val o = json.getJSONObject(it)
                    PairedPc(
                        o.getString("id"),
                        Base64.decode(o.getString("key"), Base64.NO_WRAP),
                        o.getString("name"),
                        o.getLong("added"),
                    )
                }
            }
        } catch (e: Exception) {
            emptyList()
        }
        publish(list)
    }

    private fun save(list: List<PairedPc>) {
        val json = JSONArray()
        list.forEach {
            json.put(JSONObject().put("id", it.keyIdHex).put("key", Base64.encodeToString(it.key, Base64.NO_WRAP)).put("name", it.name).put("added", it.addedMs))
        }
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, secretKey())
        val ct = c.doFinal(json.toString().toByteArray(Charsets.UTF_8))
        prefs.edit().putString("blob", Base64.encodeToString(c.iv + ct, Base64.NO_WRAP)).apply()
        publish(list)
    }

    private fun publish(list: List<PairedPc>) {
        byKeyId = list.associate { it.keyIdHex to it.key }
        _pcs.value = list
    }

    companion object {
        private const val ALIAS = "lensbridge-pairing"
        @Volatile private var instance: PairingStore? = null

        /** Process-wide instance shared by the service and the scanner. */
        fun get(context: Context): PairingStore =
            instance ?: synchronized(this) { instance ?: PairingStore(context.applicationContext).also { instance = it } }
    }
}
