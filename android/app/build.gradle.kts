plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}
android {
    namespace = "dev.sortedcord.mediadir"
    compileSdk = 35
    defaultConfig {
        applicationId = "dev.sortedcord.mediadir"
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "2.0"
        ndk { abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64") }
    }
    buildTypes {
        debug {
            applicationIdSuffix = ".dev"
            versionNameSuffix = "-dev"
            manifestPlaceholders["appLabel"] = "MediaDir Dev"
            manifestPlaceholders["appIcon"] = "@mipmap/ic_launcher_dev"
        }
        release {
            manifestPlaceholders["appLabel"] = "MediaDir"
            manifestPlaceholders["appIcon"] = "@android:drawable/ic_media_play"
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { compose = true }
}
dependencies {
    val media3 = "1.5.0"
    implementation(platform("androidx.compose:compose-bom:2024.12.01"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.media3:media3-exoplayer:$media3")
    implementation("androidx.media3:media3-ui:$media3")
    implementation("org.jellyfin.media3:media3-ffmpeg-decoder:$media3+1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("io.coil-kt:coil-compose:2.7.0")
    implementation("androidx.navigation:navigation-compose:2.8.5")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
}
