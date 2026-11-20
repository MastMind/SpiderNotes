import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 37
    namespace = "dev.spidernotes.app"

    // Release signing: the keystore file is expected at the conventional path below
    // (in CI it is decoded from the SPIDERNOTES_KEYSTORE_BASE64 secret, locally you
    // keep it there). The password comes from the SPIDERNOTES_KEYSTORE_PASSWORD env
    // var (a GitHub Actions secret in CI). Without both, the release APK is left
    // unsigned (Android will refuse to install it).
    val keystoreFile = file("../../../keystore/spidernotes-release.keystore")
    val keystorePassword = System.getenv("SPIDERNOTES_KEYSTORE_PASSWORD")
    val releaseSigned = keystoreFile.exists() && !keystorePassword.isNullOrBlank()

    signingConfigs {
        if (releaseSigned) {
            create("release") {
                storeFile = keystoreFile
                storePassword = keystorePassword ?: ""
                keyAlias = System.getenv("SPIDERNOTES_KEY_ALIAS") ?: "spidernotes"
                keyPassword = System.getenv("SPIDERNOTES_KEY_PASSWORD") ?: keystorePassword ?: ""
            }
        }
    }
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "dev.spidernotes.app"
        minSdk = 24
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (releaseSigned) {
                signingConfig = signingConfigs.getByName("release")
            }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

// Give the APK a predictable, informative name instead of the default
// "app-<flavor>-<buildtype>.apk" (e.g. "app-universal-release.apk").
// Result: spidernotes-v<version>-<abi-flavor>-<buildtype>.apk
//   e.g. spidernotes-v0.1.0-arm64-release.apk
androidComponents {
    onVariants { variant ->
        val version = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        val name = variant.name // e.g. "arm64Release", "universalRelease", "release"
        val buildType = when {
            name.endsWith("Release") -> "release"
            name.endsWith("Debug") -> "debug"
            else -> name
        }
        val flavor = name.removeSuffix("Release").removeSuffix("Debug").ifEmpty { "all" }
        val fileName = "spidernotes-v${version}-${flavor}-${buildType}.apk"
        variant.outputs.forEach { output ->
            output.outputFileName.set(fileName)
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
