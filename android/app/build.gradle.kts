plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.papercast"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.papercast"
        minSdk = 26
        targetSdk = 33
        versionCode = 1
        versionName = "0.1.0"
        // Ship only the ABIs papercast-recv-core is cross-compiled for
        // (see ../../scripts/build-recv-core.sh): arm64 device + x86_64 emulator.
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    // The prebuilt .so's live in src/main/jniLibs/<abi>/ (the default jniLibs
    // location), populated by scripts/build-recv-core.sh and gitignored.
}

dependencies {
    // Intentionally none. The shell is a plain android.app.Activity over platform
    // APIs, so it stays a thin loader with no AndroidX/Material footprint.
}
