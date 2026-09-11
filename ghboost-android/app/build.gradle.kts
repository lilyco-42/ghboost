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
        // 與 Cargo.toml 的版本保持一致（每次發版一起改）。
        // versionCode 必須**單調遞增**，否則已安裝的使用者升不了級。
        versionCode = 14
        versionName = "0.3.14"
    }

    // 簽章必須**每次建置都同一把key**，否則使用者更新時會撞
    // INSTALL_FAILED_UPDATE_INCOMPATIBLE（新舊簽章不符，只能先移除再裝）。
    // AGP 預設的 debug.keystore 是**每台機器各自現生**的 —— 也就是說
    // CI runner 每個 job 都會生一把新的，每個版本簽章都不同，使用者永遠升不了級。
    // 所以這裡用一把**固定在倉內的公開發佈 key**：
    //   - 它是公開的（密碼也是），只保證「同一個 App 的連續版本可升級」，
    //     不提供任何身分保證 —— 這對 GitHub Release 直接分發的 MIT 開源 App 是合適的。
    //   - 真的要上架 Play / 需要身分保證時，另外生一把私密 key，
    //     用下面這四個環境變數餵進來（CI 裡存成 secrets），不要進倉。
    signingConfigs {
        create("ghboostRelease") {
            storeFile = file(System.getenv("GHBOOST_KEYSTORE_FILE") ?: "../keystore/ghboost.jks")
            storePassword = System.getenv("GHBOOST_KEYSTORE_PASSWORD") ?: "ghboost"
            keyAlias = System.getenv("GHBOOST_KEY_ALIAS") ?: "ghboost"
            keyPassword = System.getenv("GHBOOST_KEY_PASSWORD") ?: "ghboost"
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.getByName("ghboostRelease")
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
