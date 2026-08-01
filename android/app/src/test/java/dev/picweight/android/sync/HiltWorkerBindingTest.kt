package dev.picweight.android.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Every `@HiltWorker` must actually be bound into [androidx.hilt.work.HiltWorkerFactory].
 *
 * ## Why this test exists
 *
 * `androidx.hilt:hilt-work` ships the `@HiltWorker` *annotation* and a `@Multibinds`
 * declaration for the worker map. The processor that fills that map lives in a separate
 * artifact, `androidx.hilt:hilt-compiler`, and it was missing from the KSP path. Note
 * that `ksp(libs.hilt.compiler)` — Dagger's compiler — is present and is a different
 * thing entirely; it processes `@HiltViewModel` and `@AssistedInject` but knows nothing
 * about `@HiltWorker`.
 *
 * The consequence was total and completely silent:
 *
 *  - `@HiltWorker` compiled without a warning, because the annotation was on the
 *    classpath.
 *  - Dagger produced `ImmutableMap.of()` — an empty worker map — because `@Multibinds`
 *    makes an empty map legal.
 *  - `HiltWorkerFactory.createWorker` is one map lookup, so it returned null for every
 *    worker.
 *  - WorkManager fell back to reflection for a `(Context, WorkerParameters)` constructor.
 *    These workers take an injected dependency as a third argument, so that threw, and
 *    the work was marked FAILED — terminally, with no backoff.
 *  - `doWork` was therefore never entered. No upload was ever attempted: the server log
 *    showed zero `POST /api/v1/meals`, not even a 4xx. The Room row kept the `QUEUED`
 *    written before enqueue and the home screen said "Waiting for a connection" forever,
 *    on a phone with a perfect signal.
 *
 * Nothing in the app observed `WorkInfo`, so the only trace was a `WM-WorkerFactory` line
 * in logcat. No unit test could catch it either, because the failure was in the build
 * graph rather than in any Kotlin the tests could call.
 *
 * ## What it checks
 *
 * The worker list is read from the source tree rather than hard-coded, so a worker added
 * later is covered automatically — the whole point being that this class of breakage is
 * invisible unless something asserts on it.
 */
class HiltWorkerBindingTest {

    /** Unit tests run with the module directory as the working directory. */
    private val sourceRoot = File("src/main/java")

    private fun hiltWorkerClassNames(): List<String> {
        assertTrue(
            "expected the Kotlin source root at ${sourceRoot.absolutePath}",
            sourceRoot.isDirectory,
        )
        return sourceRoot.walkTopDown()
            .filter { it.isFile && it.extension == "kt" }
            .mapNotNull { file ->
                val text = file.readText()
                if (!text.contains("@HiltWorker")) return@mapNotNull null
                val pkg = PACKAGE.find(text)?.groupValues?.get(1) ?: return@mapNotNull null
                // The class declared immediately after the annotation.
                val name = HILT_WORKER_CLASS.find(text)?.groupValues?.get(1) ?: return@mapNotNull null
                "$pkg.$name"
            }
            .sorted()
            .toList()
    }

    @Test
    fun `the source tree still declares the workers this app depends on`() {
        // A guard on the guard: if the walk silently found nothing, every assertion below
        // would pass vacuously and the regression would sail straight through again.
        assertEquals(
            listOf(
                "dev.picweight.android.sync.MealEventWorker",
                "dev.picweight.android.sync.MealUploadWorker",
            ),
            hiltWorkerClassNames(),
        )
    }

    /**
     * The actual regression. Both generated types must be on the runtime classpath:
     * `_AssistedFactory` is the `WorkerAssistedFactory` implementation, and `_HiltModule`
     * carries the `@Binds @IntoMap @StringKey(<class name>)` that puts it in the map.
     *
     * Delete `ksp(libs.androidx.hilt.compiler)` from app/build.gradle.kts and this fails.
     */
    @Test
    fun `every HiltWorker has its generated factory and multibinding module`() {
        val missing = hiltWorkerClassNames().flatMap { worker ->
            listOf("${worker}_AssistedFactory", "${worker}_HiltModule")
                .filter { runCatching { Class.forName(it) }.isFailure }
        }

        assertTrue(
            "Missing generated Hilt worker bindings: $missing.\n" +
                "This means androidx.hilt:hilt-compiler is not on the KSP path, so " +
                "HiltWorkerFactory's map is empty and WorkManager cannot construct " +
                "these workers at all — uploads will silently never run.",
            missing.isEmpty(),
        )
    }

    /**
     * And the binding must name the worker by the exact string WorkManager persists in
     * its WorkSpec, since the lookup is by class name.
     */
    @Test
    fun `the multibinding is keyed by the worker's fully-qualified class name`() {
        hiltWorkerClassNames().forEach { worker ->
            val module = Class.forName("${worker}_HiltModule")
            val key = module.declaredMethods
                .firstNotNullOfOrNull { method ->
                    method.annotations
                        .firstOrNull { it.annotationClass.qualifiedName == STRING_KEY }
                        ?.let { annotation ->
                            annotation.annotationClass.java
                                .getMethod("value")
                                .invoke(annotation) as String
                        }
                }
            assertEquals("binding key for $worker", worker, key)
        }
    }

    private companion object {
        val PACKAGE = Regex("""^package\s+([\w.]+)""", RegexOption.MULTILINE)
        val HILT_WORKER_CLASS = Regex("""@HiltWorker\s+(?:\w+\s+)*class\s+(\w+)""")
        const val STRING_KEY = "dagger.multibindings.StringKey"
    }
}
