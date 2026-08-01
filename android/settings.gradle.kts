import java.net.URI
import java.util.zip.ZipInputStream

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // AppAuth is published here
        maven { url = uri("https://jitpack.io") }
    }
}

rootProject.name = "picweight-android"
include(":app")

// ---------------------------------------------------------------------------
// Auto-bootstrap Android SDK if not found (runs at settings-evaluation time,
// before AGP tries to read sdk.dir). Ported from phos.
// ---------------------------------------------------------------------------
val localProps = file("local.properties")
val sdkDir: File = run {
    // 1. Check local.properties
    if (localProps.exists()) {
        val props = java.util.Properties().apply { localProps.reader().use { load(it) } }
        props.getProperty("sdk.dir")?.let { return@run file(it) }
    }
    // 2. Check environment
    System.getenv("ANDROID_HOME")?.let { return@run file(it) }
    System.getenv("ANDROID_SDK_ROOT")?.let { return@run file(it) }
    // 3. Default location
    file("${System.getProperty("user.home")}/Android/Sdk")
}

if (!file("${sdkDir}/platforms").exists()) {
    logger.lifecycle("Android SDK not found at $sdkDir — bootstrapping...")

    val os = System.getProperty("os.name").lowercase()
    val platformTag = when {
        os.contains("linux") -> "linux"
        os.contains("mac") -> "mac"
        os.contains("win") -> "win"
        else -> error("Unsupported OS: $os")
    }
    val cmdlineToolsUrl = "https://dl.google.com/android/repository/commandlinetools-${platformTag}-11076708_latest.zip"

    val toolsDir = file("${sdkDir}/cmdline-tools/latest")
    if (!file("${toolsDir}/bin/sdkmanager").exists()) {
        logger.lifecycle("Downloading command-line tools...")
        sdkDir.mkdirs()

        val tmpZip = File.createTempFile("cmdline-tools", ".zip")
        URI(cmdlineToolsUrl).toURL().openStream().use { input ->
            tmpZip.outputStream().use { input.copyTo(it) }
        }

        // Unzip
        val destParent = file("${sdkDir}/cmdline-tools")
        destParent.mkdirs()
        ZipInputStream(tmpZip.inputStream()).use { zis ->
            var entry = zis.nextEntry
            while (entry != null) {
                // Rewrite "cmdline-tools/..." -> "latest/..."
                val name = entry.name.replaceFirst("cmdline-tools/", "latest/")
                val outFile = File(destParent, name)
                if (entry.isDirectory) {
                    outFile.mkdirs()
                } else {
                    outFile.parentFile.mkdirs()
                    outFile.outputStream().use { zis.copyTo(it) }
                }
                // Preserve executable bit
                if (name.endsWith("/sdkmanager") || name.endsWith("/avdmanager")) {
                    outFile.setExecutable(true)
                }
                entry = zis.nextEntry
            }
        }
        tmpZip.delete()
        logger.lifecycle("Command-line tools installed.")
    }

    // Accept licenses
    val licensesDir = file("${sdkDir}/licenses")
    licensesDir.mkdirs()
    // These are the well-known license hashes that `yes | sdkmanager --licenses` would write
    file("${licensesDir}/android-sdk-license").writeText(
        "\n24333f8a63b6825ea9c5514f83c2829b004d1fee\n" +
        "d56f5187479451eabf01fb78af6dfcb131a6481e\n" +
        "84831b9409646a918e30573bab4c9c91346d8abd\n"
    )
    file("${licensesDir}/android-sdk-preview-license").writeText(
        "\n84831b9409646a918e30573bab4c9c91346d8abd\n"
    )
    logger.lifecycle("SDK licenses accepted.")

    // Install required components via sdkmanager. Kept identical to the versions the
    // Dockerfile's android-builder stage installs (PRD §10) so local, CI and image agree.
    val sdkmanager = file("${sdkDir}/cmdline-tools/latest/bin/sdkmanager").absolutePath
    val components = listOf("platforms;android-36", "build-tools;36.0.0", "platform-tools")
    logger.lifecycle("Installing SDK components: ${components.joinToString()}...")
    val proc = ProcessBuilder(listOf(sdkmanager) + components)
        .redirectErrorStream(true)
        .start()
    proc.inputStream.bufferedReader().forEachLine { logger.lifecycle("  $it") }
    val exitCode = proc.waitFor()
    if (exitCode != 0) error("sdkmanager failed with exit code $exitCode")
    logger.lifecycle("Android SDK ready at $sdkDir")
}

// AGP finds the SDK through local.properties or ANDROID_HOME. A pre-existing SDK with
// neither set (a fresh clone on a machine that already has one) would otherwise fail,
// so record what we resolved above. local.properties is git-ignored.
if (!localProps.exists() && System.getenv("ANDROID_HOME") == null && System.getenv("ANDROID_SDK_ROOT") == null) {
    localProps.writeText("sdk.dir=${sdkDir.absolutePath}\n")
}
