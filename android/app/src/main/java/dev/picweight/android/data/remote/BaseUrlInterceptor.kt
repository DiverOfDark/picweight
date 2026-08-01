package dev.picweight.android.data.remote

import dev.picweight.android.data.repository.AuthRepository
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.Response
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Retrofit needs a base URL at construction time, but picweight is self-hosted: the
 * server address is only known once the user has typed it on the login screen. Every
 * request is therefore built against a placeholder host and rewritten here.
 */
@Singleton
class BaseUrlInterceptor @Inject constructor(
    private val authRepository: dagger.Lazy<AuthRepository>,
) : Interceptor {

    companion object {
        const val PLACEHOLDER_BASE_URL = "http://placeholder.invalid/"
        private const val PLACEHOLDER_HOST = "placeholder.invalid"
    }

    override fun intercept(chain: Interceptor.Chain): Response {
        val original = chain.request()

        if (original.url.host != PLACEHOLDER_HOST) {
            return chain.proceed(original)
        }

        val serverUrl = authRepository.get().getServerUrl()
            ?: throw IllegalStateException("No server URL configured — cannot make API calls before login")

        val target = serverUrl.trimEnd('/').toHttpUrl()

        val newUrl = original.url.newBuilder()
            .scheme(target.scheme)
            .host(target.host)
            .port(target.port)
            .build()

        return chain.proceed(original.newBuilder().url(newUrl).build())
    }
}
