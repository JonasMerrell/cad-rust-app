plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.rcad.capture"
    compileSdk = 35

    defaultConfig {
        applicationId = "org.rcad.capture"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"
    }

    // A fixed, non-secret signing key so every build (CI or local) installs
    // over the previous one. It protects nothing; it only keeps the signature
    // stable for sideloading.
    signingConfigs {
        create("sideload") {
            storeFile = file("sideload.keystore")
            storePassword = "rcadcapture"
            keyAlias = "rcadcapture"
            keyPassword = "rcadcapture"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("sideload")
        }
        debug {
            signingConfig = signingConfigs.getByName("sideload")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    val camerax = "1.4.1"
    implementation("androidx.camera:camera-core:$camerax")
    implementation("androidx.camera:camera-camera2:$camerax")
    implementation("androidx.camera:camera-lifecycle:$camerax")
    implementation("androidx.camera:camera-view:$camerax")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("androidx.work:work-runtime:2.10.0")
    implementation("androidx.annotation:annotation-experimental:1.4.1")
}
