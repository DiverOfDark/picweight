package dev.picweight.android.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.util.Log
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Whether this phone actually has a usable route to the internet.
 *
 * Exists so the UI can stop guessing. "Waiting for a connection" used to be printed for
 * every meal sitting in the upload queue, whatever the reason — including a queue that
 * was permanently broken on a phone with five bars of LTE. That one sentence is only
 * honest when it is true, so it now has to be backed by an actual answer from
 * [ConnectivityManager].
 *
 * [NetworkCapabilities.NET_CAPABILITY_VALIDATED] is deliberately part of the test: a
 * captive-portal Wi-Fi that has not been signed into is a network the phone is attached
 * to and cannot reach the server through, which is exactly the case a naive
 * "is there a network" check gets wrong.
 */
@Singleton
class ConnectivityMonitor @Inject constructor(
    @param:ApplicationContext private val context: Context,
) {
    private val manager: ConnectivityManager?
        get() = context.getSystemService(ConnectivityManager::class.java)

    /** A point-in-time answer, for callers that cannot hold a subscription. */
    fun isOnline(): Boolean {
        val cm = manager ?: return false
        val caps = runCatching { cm.getNetworkCapabilities(cm.activeNetwork) }.getOrNull() ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }

    /**
     * Emits the current answer, then again on every change.
     *
     * Assumes `ACCESS_NETWORK_STATE`, which the manifest declares. If registration ever
     * fails anyway, this reports *online* rather than offline: claiming the phone is
     * offline when we do not know is the exact lie this class was written to stop.
     */
    val online: Flow<Boolean> = callbackFlow {
        trySend(isOnline())

        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                trySend(isOnline())
            }

            override fun onLost(network: Network) {
                trySend(isOnline())
            }

            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                trySend(isOnline())
            }
        }

        val cm = manager
        val registered = cm != null &&
            runCatching { cm.registerDefaultNetworkCallback(callback) }
                .onFailure { Log.w(TAG, "Cannot observe connectivity; assuming online", it) }
                .isSuccess
        if (!registered) trySend(true)

        awaitClose {
            if (cm != null && registered) runCatching { cm.unregisterNetworkCallback(callback) }
        }
    }.distinctUntilChanged()

    private companion object {
        const val TAG = "ConnectivityMonitor"
    }
}
