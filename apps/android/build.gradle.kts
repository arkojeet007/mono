// SPDX-License-Identifier: MPL-2.0

plugins {
    id("com.android.application") version "9.4.1" apply false
    // Never applied: AGP 9 compiles Kotlin itself and refuses this plugin in
    // a module. Declaring it here only pins the Kotlin it uses to 2.4.21,
    // above the 2.2.10 AGP 9.4.1 brings.
    id("org.jetbrains.kotlin.android") version "2.4.21" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21" apply false
}
