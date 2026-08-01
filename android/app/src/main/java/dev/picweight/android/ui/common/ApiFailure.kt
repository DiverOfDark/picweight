package dev.picweight.android.ui.common

import android.util.Log
import com.fasterxml.jackson.core.JsonProcessingException
import com.fasterxml.jackson.databind.JsonMappingException
import retrofit2.HttpException
import java.io.IOException
import java.io.InterruptedIOException
import javax.net.ssl.SSLException

/**
 * What actually went wrong with a call, at the granularity the user — and whoever is
 * reading `adb logcat` six weeks later — needs.
 *
 * These exist because "Offline" used to be the answer to every failure. A 200 whose body
 * the app could not deserialise, a 500, an expired session and a genuine loss of network
 * were indistinguishable both on screen and in the log, and diagnosing the difference
 * cost hours. Each member below is a case a user can act on differently.
 */
enum class FailureKind {
    /**
     * No route to the server: airplane mode, no DNS, dead Wi-Fi. This is the **only**
     * case where "showing what this phone knows" is true, because it is the only case
     * where the request never left and Room is genuinely the best available answer.
     */
    OFFLINE,

    /** The request left and nothing came back in time. Reachable-but-wedged, not offline. */
    TIMEOUT,

    /** TLS refused. Something answered on that address, so this is a cert problem. */
    INSECURE,

    /** 401/403 — the session, not the network. */
    UNAUTHORIZED,

    /** Any other non-2xx. The status code is carried in the message; it is the handle. */
    HTTP,

    /**
     * The server answered 2xx and the app could not read the body. Never "offline": the
     * round trip completed. This is the client and the server disagreeing about the wire
     * format, and saying so by name is the whole point of this enum.
     */
    CONTRACT,

    /** Not a transport failure at all. Reported verbatim rather than guessed at. */
    UNEXPECTED,
}

/**
 * A classified failure and the one line the user should see for it.
 *
 * [status] is the HTTP status when the server actually answered, and null otherwise. It
 * is carried separately from [message] because some callers need to *branch* on a
 * specific code rather than print it: a 409 from `POST /meals/{id}/retry` is not a
 * malfunction — it means the row that was tapped is stale — and only the caller knows
 * that. Parsing the digits back out of the sentence would be the alternative.
 */
data class ApiFailure(val kind: FailureKind, val message: String, val status: Int? = null)

/**
 * Turns a thrown [Throwable] into an honest, user-facing sentence — and a logcat entry.
 *
 * Call [report] from a ViewModel's `onFailure`: it logs the underlying exception (with
 * its stack trace for everything that isn't a plain outage) and hands back the message
 * to render. The UI stays short; the log keeps the truth.
 */
object ApiFailures {

    /**
     * Deserialisation failures from serializers that are *not* on this build's classpath.
     *
     * The generated models use Jackson (`serializationLibrary=jackson` in
     * `app/build.gradle.kts`), so [JsonProcessingException] covers today's reality and
     * these can only be matched by name. They are listed anyway so that switching the
     * generator's serializer keeps parse failures classified as a contract mismatch
     * instead of silently degrading back into "offline".
     */
    private val FOREIGN_PARSE_FAILURES = setOf(
        "com.squareup.moshi.JsonDataException",
        "com.squareup.moshi.JsonEncodingException",
        "kotlinx.serialization.SerializationException",
    )

    /** Cause chains are shallow in practice; the bound just guarantees termination. */
    private const val MAX_CAUSE_DEPTH = 8

    /** Classifies [t] without producing any copy. */
    fun classify(t: Throwable): FailureKind = explain(t).kind

    /** Classifies [t] and produces the sentence for it. */
    fun explain(t: Throwable): ApiFailure {
        // Retrofit hands the converter's exception over untouched, but a repository or a
        // coroutine helper may have wrapped it. Classify the first link that is
        // recognisable rather than the outermost one, or a wrapper turns every failure
        // into "unexpected".
        causeChain(t).forEach { link -> describe(link)?.let { return it } }
        return ApiFailure(FailureKind.UNEXPECTED, "Something went wrong: ${label(t)}")
    }

    /**
     * Logs [t] under [tag] and returns the failure to render.
     *
     * [what] names the operation in the log line ("Refreshing today"), so a logcat entry
     * is readable without knowing which call site produced it.
     */
    fun report(tag: String, what: String, t: Throwable): ApiFailure {
        val failure = explain(t)
        if (failure.kind == FailureKind.OFFLINE) {
            // Expected and self-explanatory; a stack trace per lost connection is noise.
            Log.w(tag, "$what — offline: ${label(t)}", t)
        } else {
            // Everything else is a bug in one of the two halves. The stack trace is the
            // part that names the field, the URL and the status.
            Log.e(tag, "$what — ${failure.kind}: ${failure.message}", t)
        }
        return failure
    }

    private fun causeChain(t: Throwable): Sequence<Throwable> =
        generateSequence(t) { link -> link.cause?.takeIf { it !== link } }.take(MAX_CAUSE_DEPTH)

    /** Classifies one link of a cause chain, or null if it says nothing useful. */
    private fun describe(t: Throwable): ApiFailure? = when {
        // MUST be tested before the IOException arms: every Jackson failure *is* an
        // IOException (JsonProcessingException extends it). Checking IOException first
        // is exactly the mistake that reported a 200-with-an-unparseable-body as
        // "Offline — showing what this phone knows."
        isParseFailure(t) -> ApiFailure(FailureKind.CONTRACT, contractMessage(t))

        t is HttpException -> httpFailure(t.code())

        t is SSLException -> ApiFailure(
            FailureKind.INSECURE,
            "Couldn't set up a secure connection to the server — its certificate was rejected.",
        )

        // SocketTimeoutException and friends. The server may be perfectly reachable and
        // simply stuck, so this must not claim the phone is offline.
        t is InterruptedIOException -> ApiFailure(
            FailureKind.TIMEOUT,
            "The server didn't answer in time.",
        )

        // UnknownHostException, ConnectException, NoRouteToHostException and the rest of
        // the "it never left the phone" family.
        t is IOException -> ApiFailure(
            FailureKind.OFFLINE,
            "Offline — couldn't reach the server.",
        )

        else -> null
    }

    private fun httpFailure(code: Int): ApiFailure = when {
        code == 401 || code == 403 -> ApiFailure(
            FailureKind.UNAUTHORIZED,
            "The server rejected this session (HTTP $code). Sign in again.",
            code,
        )

        code == 404 -> ApiFailure(
            FailureKind.HTTP,
            "The server has no such endpoint or record (HTTP 404).",
            code,
        )

        code == 408 || code == 429 -> ApiFailure(
            FailureKind.HTTP,
            "The server asked us to slow down (HTTP $code).",
            code,
        )

        // A conflict is the server declining because the *state* moved, not because
        // anything broke — whoever asked is holding a stale view of it.
        code == 409 -> ApiFailure(
            FailureKind.HTTP,
            "That no longer applies — the server has moved on (HTTP 409).",
            code,
        )

        code in 500..599 -> ApiFailure(
            FailureKind.HTTP,
            "The server failed on its side (HTTP $code).",
            code,
        )

        else -> ApiFailure(FailureKind.HTTP, "The server refused that (HTTP $code).", code)
    }

    private fun isParseFailure(t: Throwable): Boolean =
        t is JsonProcessingException || t.javaClass.name in FOREIGN_PARSE_FAILURES

    /**
     * Names the failure for what it is. "Contract mismatch" is deliberate wording: the
     * server said 200, so the only thing wrong is that the two halves disagree about the
     * shape of the payload — the exact condition that a naive "Offline" hides.
     */
    private fun contractMessage(t: Throwable): String {
        val at = jsonPath(t)?.let { " at `$it`" }.orEmpty()
        return "Client/server contract mismatch — the server replied, but this app " +
            "couldn't read the response$at. Not an outage: the app and the server " +
            "disagree about the API. Update whichever is older."
    }

    /**
     * The dotted JSON path Jackson accumulated while unwinding, e.g. `user.created_at` —
     * which is the single most useful fact about a contract mismatch.
     */
    private fun jsonPath(t: Throwable): String? = (t as? JsonMappingException)
        ?.path
        ?.takeIf { it.isNotEmpty() }
        ?.joinToString(".") { it.fieldName ?: "[${it.index}]" }

    private fun label(t: Throwable): String =
        t.message?.takeIf { it.isNotBlank() }
            ?.let { "${t.javaClass.simpleName}: $it" }
            ?: t.javaClass.simpleName
}
