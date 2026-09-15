plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.anadolupanteri.uzakel"
    compileSdk = 34

    defaultConfig {
        applicationId = "org.anadolupanteri.uzakel"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    buildFeatures {
        compose = true
    }
    composeOptions {
        kotlinCompilerExtensionVersion = "1.5.14"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.6")
    implementation("androidx.activity:activity-compose:1.9.2")

    implementation(platform("androidx.compose:compose-bom:2024.09.03"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")

    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")

    // X25519 + ChaCha20-Poly1305 for the end-to-end encrypted session
    // (ARCHITECTURE.md §2.3.1). Android's own javax.crypto/KeyPairGenerator
    // only gets X25519 support on API 33+; minSdk here is 26, so Bouncy
    // Castle's raw primitives are used instead — chosen specifically
    // because they're low-level (no library-owned key/wire format
    // wrapping it), which is what makes it possible to match
    // `daemon/src/crypto.rs`'s byte-for-byte HKDF/AEAD derivation exactly.
    implementation("org.bouncycastle:bcprov-jdk18on:1.78.1")

    // Camera preview + frame analysis for QR-code pairing (scan the
    // BacakOS panel's QR instead of typing a PIN + IP by hand).
    val cameraxVersion = "1.3.4"
    implementation("androidx.camera:camera-core:$cameraxVersion")
    implementation("androidx.camera:camera-camera2:$cameraxVersion")
    implementation("androidx.camera:camera-lifecycle:$cameraxVersion")
    implementation("androidx.camera:camera-view:$cameraxVersion")
    // ZXing's decoder core, not Google's ML Kit: pure-Java, no Google Play
    // Services dependency at all (ML Kit's on-device barcode scanner still
    // needs Play Services present to fetch/run its model) — this app talks
    // to nothing but the LAN everywhere else, and pairing shouldn't be the
    // one feature that silently fails on a Play-Services-less device.
    implementation("com.google.zxing:core:3.5.3")

    debugImplementation("androidx.compose.ui:ui-tooling")
}
