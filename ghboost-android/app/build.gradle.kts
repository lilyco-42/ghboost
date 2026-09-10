plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.ghboost.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.ghboost.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            // 未簽名的 APK 在 Android 上**根本裝不起來** —— 使用者只會看到
            // 「應用程式未安裝」，不會有任何有用的錯誤。也就是說在加這一行之前，
            // 每個版本掛上去的 4 個 `*-unsigned.apk` 沒有人能用。
            // 拿到正式上架憑證前，先沿用 debug key 讓 release 包可安裝
            // （AGP 會自動生 ~/.android/debug.keystore，CI 上也一樣）。
            // 有正式憑證時：建 signingConfigs.create("release") 指過去即可。
            signingConfig = signingConfigs.getByName("debug")
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    sourceSets {
        getByName("main") {
            // Rust .so 放在 jniLibs/ 由 CI 注入
        }
    }

    splits {
        abi {
            isEnable = true
            reset()
            include("arm64-v8a", "armeabi-v7a", "x86_64")
            isUniversalApk = true
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.constraintlayout:constraintlayout:2.2.1")
    implementation("androidx.lifecycle:lifecycle-viewmodel-ktx:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
}
