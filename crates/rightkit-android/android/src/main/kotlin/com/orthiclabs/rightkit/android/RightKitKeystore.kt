package com.orthiclabs.rightkit.android

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.GeneralSecurityException
import java.security.KeyStore
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * AES-256-GCM with a key that never leaves `AndroidKeyStore`. Implements the contract documented in
 * rightkit-credentials `src/android.rs` (module "The injected bridge"); the native side calls the
 * two `@JvmStatic` methods by name.
 *
 * Blob layout: 12-byte IV chosen by the Keystore, then ciphertext and 16-byte tag.
 * The key requires no user authentication, so a background caller never prompts.
 */
internal object RightKitKeystore {
    // Mirrored in crates/rightkit-android/src/keystore.rs (a host test checks them).
    const val CODE_NO_CONTEXT = 1
    const val CODE_KEY_MISSING = 2
    const val CODE_AUTH_FAILED = 3
    const val CODE_FAILED = 100

    private const val PROVIDER = "AndroidKeyStore"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val IV_BYTES = 12
    private const val TAG_BITS = 128

    private val lock = Any()

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    private fun existingKey(alias: String): SecretKey? =
        keyStore().getKey(alias, null) as? SecretKey

    private fun getOrCreateKey(alias: String): SecretKey = synchronized(lock) {
        existingKey(alias) ?: run {
            val spec = KeyGenParameterSpec.Builder(
                alias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .build()
            KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER).run {
                init(spec)
                generateKey()
            }
        }
    }

    @JvmStatic
    fun encrypt(alias: String, plaintext: ByteArray, aad: ByteArray): ByteArray {
        try {
            val key = getOrCreateKey(alias)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, key) // the Keystore picks the IV
            cipher.updateAAD(aad)
            val body = cipher.doFinal(plaintext)
            return cipher.iv + body
        } catch (e: RightKitBridgeException) {
            throw e
        } catch (e: Exception) {
            throw RightKitBridgeException(CODE_FAILED)
        }
    }

    @JvmStatic
    fun decrypt(alias: String, blob: ByteArray, aad: ByteArray): ByteArray {
        try {
            if (blob.size < IV_BYTES + TAG_BITS / 8) throw RightKitBridgeException(CODE_AUTH_FAILED)
            val key = synchronized(lock) { existingKey(alias) }
                ?: throw RightKitBridgeException(CODE_KEY_MISSING)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                key,
                GCMParameterSpec(TAG_BITS, blob, 0, IV_BYTES),
            )
            cipher.updateAAD(aad)
            return cipher.doFinal(blob, IV_BYTES, blob.size - IV_BYTES)
        } catch (e: RightKitBridgeException) {
            throw e
        } catch (e: AEADBadTagException) {
            throw RightKitBridgeException(CODE_AUTH_FAILED)
        } catch (e: GeneralSecurityException) {
            // A Keystore key that can no longer decrypt (for example after a lock-screen reset
            // invalidated it) surfaces here; report it as a generic failure, never the message.
            throw RightKitBridgeException(CODE_FAILED)
        } catch (e: Exception) {
            throw RightKitBridgeException(CODE_FAILED)
        }
    }
}
