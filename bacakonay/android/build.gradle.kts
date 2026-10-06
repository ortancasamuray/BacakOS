// Same AGP/Kotlin/Compose versions as uzakel-android, so both BacakOS phone
// apps build with one toolchain.
plugins {
    id("com.android.application") version "8.5.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
}
