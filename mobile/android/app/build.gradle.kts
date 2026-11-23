plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.rcad.mobile"
    compileSdk = 35

    defaultConfig {
        applicationId = "org.rcad.mobile"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"
    }

    // The same fixed, non-secret sideload key as rCAD Capture, so every build (CI or local)
    // installs over the previous one. It protects nothing.
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
    implementation("androidx.activity:activity-ktx:1.9.3")
}
