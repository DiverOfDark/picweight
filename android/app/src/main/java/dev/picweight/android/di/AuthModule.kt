@file:Suppress("DEPRECATION") // see AuthModule's kdoc

package dev.picweight.android.di

import android.content.Context
import android.content.SharedPreferences
import android.util.Log
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import java.security.KeyStore
import javax.inject.Named
import javax.inject.Singleton

/**
 * The store the session JWT and the AppAuth refresh token live in.
 *
 * Refresh tokens are long-lived credentials for the user's identity provider, so they
 * go in EncryptedSharedPreferences under an AndroidKeyStore master key rather than in
 * plain prefs (PRD §7).
 *
 * `androidx.security:security-crypto` is deprecated upstream with no drop-in successor
 * for SharedPreferences. The alternatives are a hand-rolled KeyStore wrapper — worse,
 * because it would be crypto written here — or plaintext prefs, which is not an option
 * for a refresh token. Keeping the deprecated API is the honest choice until Jetpack
 * ships a replacement; revisit then.
 */
@Module
@InstallIn(SingletonComponent::class)
object AuthModule {

    private const val TAG = "AuthModule"
    private const val PREFS_NAME = "picweight_auth_prefs"

    @Provides
    @Singleton
    @Named("auth")
    fun provideEncryptedPreferences(@ApplicationContext context: Context): SharedPreferences {
        return try {
            createEncryptedPrefs(context)
        } catch (e: Exception) {
            // A rotated or lost master key makes the file undecryptable. Losing the
            // session is recoverable (log in again); refusing to start is not.
            Log.w(TAG, "Encrypted prefs corrupted, resetting", e)
            context.deleteSharedPreferences(PREFS_NAME)
            try {
                val keyStore = KeyStore.getInstance("AndroidKeyStore")
                keyStore.load(null)
                keyStore.deleteEntry(MasterKey.DEFAULT_MASTER_KEY_ALIAS)
            } catch (ke: Exception) {
                Log.w(TAG, "Failed to delete master key", ke)
            }
            createEncryptedPrefs(context)
        }
    }

    private fun createEncryptedPrefs(context: Context): SharedPreferences {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()

        return EncryptedSharedPreferences.create(
            context,
            PREFS_NAME,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }
}
