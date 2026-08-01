plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.hilt)
    alias(libs.plugins.ksp)
    id("org.openapi.generator") version "7.20.0"
}

android {
    namespace = "dev.picweight.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.picweight.android"
        minSdk = 26
        targetSdk = 36
        versionCode = (project.findProperty("versionCode") as String?)?.toIntOrNull() ?: 1
        versionName = (project.findProperty("versionName") as String?) ?: "1.0.0"

        manifestPlaceholders["appAuthRedirectScheme"] = "dev.picweight.android"
    }

    signingConfigs {
        // Signing is opt-in: the Dockerfile's android-builder stage and the CI docker job
        // pass KEYSTORE_PASSWORD as a BuildKit secret, and an unsigned APK is produced
        // when they don't. Generate the keystore once and keep it out of the image:
        //   keytool -genkeypair -v -keystore android/picweight-release.keystore \
        //     -alias picweight -keyalg RSA -keysize 4096 -validity 10000
        // The store password and the key password must be the same value, because that
        // is the single secret the build stage is given.
        create("release") {
            // Blank, not just null: GitHub Actions sets `env:` entries fed from an
            // absent secret to the *empty string*, so `!= null` would switch signing
            // on with no keystore and fail `validateSigningRelease`. This matches the
            // Dockerfile's `[ -s /run/secrets/keystore_password ]`.
            val password = System.getenv("KEYSTORE_PASSWORD")
            if (!password.isNullOrBlank()) {
                storeFile = rootProject.file("picweight-release.keystore")
                storePassword = password
                keyAlias = "picweight"
                keyPassword = password
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            if (!System.getenv("KEYSTORE_PASSWORD").isNullOrBlank()) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

// The local JDK is 25; CI's `build-android` job and the Dockerfile's android-builder
// stage both run `eclipse-temurin:17-jdk`. Pin the toolchain so all three compile
// against the same JDK rather than silently producing different bytecode.
kotlin {
    jvmToolchain(17)
}

// OpenAPI Generator configuration.
//
// The wire models are GENERATED from the utoipa-exported spec — never hand-written —
// so the API contract cannot drift (PRD §7 "Android"). Only models are generated; the
// Retrofit interface itself is hand-written in PicweightApi.kt because multipart ingest
// and the SSE stream need shapes the generator does not express well.
openApiGenerate {
    generatorName.set("java")
    inputSpec.set("${rootProject.projectDir}/openapi.json")
    outputDir.set("${layout.buildDirectory.get().asFile}/generated/openapi")
    apiPackage.set("dev.picweight.android.data.remote.api")
    modelPackage.set("dev.picweight.android.data.remote.model")
    invokerPackage.set("dev.picweight.android.data.remote")
    skipValidateSpec.set(true)
    configOptions.set(mapOf(
        "library" to "retrofit2",
        "useCoroutines" to "true",
        "serializationLibrary" to "jackson",
        "dateLibrary" to "java8",
        "sourceFolder" to "src/main/java",
        "useJakartaEe" to "false",
        "openApiNullable" to "false",
        "documentationProvider" to "none",
        "annotationLibrary" to "none",
    ))
    globalProperties.set(mapOf(
        "models" to "",
        "apis" to "false",
    ))
}

// Add generated sources to build
android {
    sourceSets {
        getByName("main") {
            java.directories.add("${layout.buildDirectory.get().asFile}/generated/openapi/src/main/java")
        }
    }
}

tasks.named("preBuild") {
    dependsOn("openApiGenerate")
}

tasks.configureEach {
    if (name.startsWith("ksp") && name.endsWith("Kotlin")) {
        dependsOn("openApiGenerate")
    }
}

dependencies {
    // Compose
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.compose.foundation)
    implementation(libs.compose.icons.extended)
    implementation(libs.compose.activity)
    debugImplementation(libs.compose.ui.tooling)

    // Lifecycle
    implementation(libs.lifecycle.runtime)
    implementation(libs.lifecycle.viewmodel)
    implementation(libs.lifecycle.process)

    // Navigation
    implementation(libs.navigation.compose)

    // Hilt
    implementation(libs.hilt.android)
    ksp(libs.hilt.compiler)
    implementation(libs.hilt.navigation.compose)
    implementation(libs.hilt.work)

    // Generated code annotations
    compileOnly("javax.annotation:javax.annotation-api:1.3.2")

    // Networking
    implementation(libs.okhttp)
    implementation(libs.okhttp.logging)
    implementation(libs.okhttp.sse)
    implementation(libs.retrofit)
    implementation(libs.retrofit.jackson)
    implementation(libs.retrofit.scalars)
    implementation(libs.jackson.databind)
    implementation(libs.jackson.kotlin)
    implementation(libs.jackson.jsr310)

    // Images
    implementation(libs.coil.compose)
    implementation(libs.coil.network.okhttp)

    // Camera + on-device barcode
    implementation(libs.camera.core)
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.exifinterface)
    implementation(libs.mlkit.barcode)

    // Auth
    implementation(libs.appauth)
    implementation(libs.security.crypto)

    // Local source of truth
    implementation(libs.room.runtime)
    implementation(libs.room.ktx)
    ksp(libs.room.compiler)

    // WorkManager
    implementation(libs.work.runtime)

    // Core
    implementation(libs.core.ktx)
    implementation(libs.core.splashscreen)

    // Testing
    testImplementation(libs.junit)
    testImplementation(libs.mockk)
    testImplementation(libs.coroutines.test)
}
