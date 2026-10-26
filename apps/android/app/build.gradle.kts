// SPDX-License-Identifier: MPL-2.0

import java.util.zip.ZipFile
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

fun koruGit(vararg args: String): String = runCatching {
    val process = ProcessBuilder("git", *args)
        .directory(rootDir)
        .redirectErrorStream(true)
        .start()
    val output = process.inputStream.bufferedReader().readText().trim()
    if (process.waitFor() == 0) output else ""
}.getOrDefault("")

// The version is the workspace's: the root Cargo.toml's [workspace.package]
// version, so a phone and a desktop built from one tree agree on
// MAJOR.MINOR.PATCH, the numbers the linked-device drift rule compares.
fun koruWorkspaceVersion(): String {
    val text = rootDir.resolve("../..").canonicalFile.resolve("Cargo.toml").readText()
    val section = text.substringAfter("[workspace.package]").substringBefore("\n[")
    return Regex("""^version = "([^"]+)"""", RegexOption.MULTILINE).find(section)?.groupValues?.get(1)
        ?: error("no [workspace.package] version in the root Cargo.toml")
}

fun koruGitBranch(): String = koruGit("rev-parse", "--abbrev-ref", "HEAD").takeIf { it != "HEAD" } ?: ""
fun koruGitCommit(): String = koruGit("rev-parse", "--short=12", "HEAD")

fun accountBuildValue(name: String): String {
    val value = providers.gradleProperty(name).orNull.orEmpty()
    require(value.length <= 2048 && value.none { it.isISOControl() }) { "invalid account build configuration" }
    return "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
}

android {
    namespace = "app.koru.mobile"
    compileSdk = 37
    ndkVersion = "30.0.15729638"

    defaultConfig {
        applicationId = "do.koru.app"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = koruWorkspaceVersion()
        testInstrumentationRunner = "app.koru.mobile.IsolatedUiTestRunner"
        vectorDrawables.useSupportLibrary = true
        // The git branch and commit this app was built from, advertised to
        // linked devices beside versionName so a mixed-build mesh names itself
        // — the commit is what decides "same code" against a desktop whose
        // version string is not comparable. Empty when git is unavailable,
        // and the device then advertises neither.
        buildConfigField("String", "GIT_BRANCH", "\"${koruGitBranch()}\"")
        buildConfigField("String", "GIT_COMMIT", "\"${koruGitCommit()}\"")
        // Public service coordinates, supplied by the authorized build. No
        // intent, document or hosted response can choose this configuration.
        buildConfigField("String", "ACCOUNT_ORIGIN", accountBuildValue("koruAccountOrigin"))
        buildConfigField("String", "ACCOUNT_ISSUER", accountBuildValue("koruAccountIssuer"))
        buildConfigField("String", "ACCOUNT_CREDENTIAL_ROLLOUT", accountBuildValue("koruAccountCredentialRollout"))
        buildConfigField("String", "GOOGLE_SERVER_CLIENT_ID", accountBuildValue("koruGoogleServerClientId"))
        buildConfigField("String", "ACCOUNT_CHANNEL", accountBuildValue("koruAccountChannel"))
        // A local object worker this build may send blob bytes to, beside the
        // compiled-in per-region set (`cloud-blob-object-v1` §2). Empty on
        // every build that is not pointed at somebody's development stack.
        //
        // A phone has no environment, so it cannot be told what a desktop
        // reads from KORU_CLOUD_BLOB_BYTE_ORIGINS; without it a phone is
        // handed its own stack's byte URL and refuses it, which reads as a
        // broken transfer engine. Naming a public host here widens nothing:
        // the allowlist admits only a loopback, private-range or link-local
        // address, and refuses the value with the production set standing.
        buildConfigField("String", "BLOB_BYTE_ORIGINS", accountBuildValue("koruBlobByteOrigins"))
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets.named("main") {
        java.srcDir("../../../contracts/gen/kotlin")
        // The three fonts Theme.kt loads, at the root of the APK's assets by
        // their file names. Only these: the desktop set beside them in
        // assets/fonts/ stays out of the APK. Fetched, not committed:
        // `koru.bat fonts` or `./koru.sh fonts` at the repository root.
        assets.srcDir("../../../assets/fonts/android")
        // A File, not a Provider: AGP 9 refuses providers in the source set
        // API. The native tasks below write here, and mergeDebugNativeLibs
        // depends on them, so the provider's task wiring is not needed.
        jniLibs.srcDir(layout.buildDirectory.dir("generated/jniLibs").get().asFile)
    }
    sourceSets.named("test") {
        resources.srcDir("../../../contracts/testdata")
        resources.srcDir("src/main/res")
    }

    packaging.resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
}

kotlin.compilerOptions.jvmTarget.set(JvmTarget.JVM_17)

data class KoruNativeTarget(
    val taskSuffix: String,
    val rustTarget: String,
    val abi: String,
    val clangPrefix: String,
)

val koruRepoRoot = rootDir.resolve("../..").canonicalFile
val koruAndroidSdk = providers.environmentVariable("ANDROID_HOME")
    .orElse(providers.environmentVariable("ANDROID_SDK_ROOT"))
val koruNdkVersion = "30.0.15729638"
val koruHostTag = when {
    System.getProperty("os.name").startsWith("Windows", ignoreCase = true) -> "windows-x86_64"
    System.getProperty("os.name").startsWith("Mac", ignoreCase = true) -> "darwin-x86_64"
    else -> "linux-x86_64"
}
val koruCommandSuffix = if (koruHostTag.startsWith("windows")) ".cmd" else ""
val koruExeSuffix = if (koruHostTag.startsWith("windows")) ".exe" else ""
val koruNativeTargets = listOf(
    KoruNativeTarget("Arm64", "aarch64-linux-android", "arm64-v8a", "aarch64-linux-android29"),
    KoruNativeTarget("X8664", "x86_64-linux-android", "x86_64", "x86_64-linux-android29"),
)

val koruNativeTasks = koruNativeTargets.map { target ->
    tasks.register<Exec>("buildKoruNative${target.taskSuffix}") {
        group = "build"
        description = "Builds koru_mobile for Android ${target.abi}."
        workingDir(koruRepoRoot)

        val sdk = koruAndroidSdk.orNull.orEmpty()
        val toolchain = file("$sdk/ndk/$koruNdkVersion/toolchains/llvm/prebuilt/$koruHostTag/bin")
        val clang = toolchain.resolve("${target.clangPrefix}-clang$koruCommandSuffix")
        val clangxx = toolchain.resolve("${target.clangPrefix}-clang++$koruCommandSuffix")
        val ar = toolchain.resolve("llvm-ar$koruExeSuffix")
        val targetEnv = target.rustTarget.replace('-', '_')
        val cargoTargetEnv = target.rustTarget.replace('-', '_').uppercase()

        doFirst {
            require(sdk.isNotBlank()) { "ANDROID_HOME or ANDROID_SDK_ROOT must name an Android SDK" }
            require(clang.isFile) { "Android NDK compiler not found: $clang" }
        }
        environment("CARGO_TARGET_${cargoTargetEnv}_LINKER", clang.absolutePath)
        environment("CARGO_TARGET_${cargoTargetEnv}_AR", ar.absolutePath)
        environment("CC_$targetEnv", clang.absolutePath)
        environment("CXX_$targetEnv", clangxx.absolutePath)
        environment("AR_$targetEnv", ar.absolutePath)
        commandLine("cargo", "build", "-p", "koru-mobile-native", "--target", target.rustTarget)

        doLast {
            // Cargo honours CARGO_TARGET_DIR, so the library is found there too.
            val cargoTarget = System.getenv("CARGO_TARGET_DIR")?.takeIf { it.isNotBlank() }?.let(::File)
                ?: koruRepoRoot.resolve("target")
            val library = cargoTarget.resolve("${target.rustTarget}/debug/libkoru_mobile.so")
            require(library.isFile) { "Cargo did not produce $library" }
            copy {
                from(library)
                into(layout.buildDirectory.dir("generated/jniLibs/${target.abi}"))
            }
        }
    }
}

tasks.configureEach {
    if (name == "mergeDebugNativeLibs") dependsOn(koruNativeTasks)
}

// **An APK without its fonts must not build.** The asset source above once
// named a directory that had been renamed away, and every build after that
// shipped without the default font and said nothing.
val koruMobileFonts = listOf("Inter-Regular.ttf", "Inter-Medium.ttf", "Manrope-SemiBold.ttf")
    .map { koruRepoRoot.resolve("assets/fonts/android/$it") }
val checkKoruMobileFonts = tasks.register("checkKoruMobileFonts") {
    group = "verification"
    description = "Fails when a font Theme.kt loads is missing from assets/fonts/android."
    doLast {
        val missing = koruMobileFonts.filterNot { it.isFile }
        if (missing.isNotEmpty()) {
            throw GradleException(
                "missing fonts: ${missing.joinToString { it.name }}. " +
                    "Run `koru.bat fonts` (Windows) or `./koru.sh fonts` at the repository root.",
            )
        }
    }
}
tasks.configureEach {
    if (Regex("merge\\w+Assets").matches(name) && !name.contains("Test")) dependsOn(checkKoruMobileFonts)
}

// **The notices the APK owes, and where its source is (MPL-2.0 §3.2).** Every
// variant gets `assets/licenses/`: Koru's LICENSE and NOTICE, SOURCE.txt with
// https://github.com/koruworks/mono/tree/<commit>, and THIRD-PARTY-NOTICES.txt
// from `koru-release notices android`. That one generator (the desktop's)
// covers the native library's crates and the bundled faces; this build adds
// the part only Gradle can see, the Maven libraries of the variant's runtime
// classpath: what each POM declares, and the license files each artifact
// carries. Settings -> Open-source licenses shows them. A debug build labels
// a crate without a recorded license instead of refusing it; a release does not.
val koruLicenseFiles = listOf("LICENSE.txt", "NOTICE.txt", "SOURCE.txt", "THIRD-PARTY-NOTICES.txt")

abstract class KoruNoticesTask : DefaultTask() {
    @get:org.gradle.api.tasks.Internal
    abstract val classpath: Property<Configuration>

    @get:org.gradle.api.tasks.Input
    abstract val debugBuild: Property<Boolean>

    @get:org.gradle.api.tasks.Input
    abstract val sourceUrl: Property<String>

    @get:org.gradle.api.tasks.Internal
    abstract val repoRoot: Property<File>

    @get:org.gradle.api.tasks.OutputDirectory
    abstract val outputDir: DirectoryProperty

    init {
        // Cargo decides what is current (its lockfile and the crate records),
        // so the task always runs; it costs one `cargo metadata`.
        outputs.upToDateWhen { false }
    }

    @org.gradle.api.tasks.TaskAction
    fun generate() {
        val root = repoRoot.get()
        val out = outputDir.get().asFile.resolve("licenses")
        out.deleteRecursively()
        out.mkdirs()
        val maven = temporaryDir.resolve("maven-libraries.json")
        maven.writeText(groovy.json.JsonOutput.toJson(mavenLibraries()))

        val command = mutableListOf(
            "cargo", "run", "--quiet", "--locked", "-p", "koru-release", "--",
            "notices", "android",
            "--out", out.resolve("THIRD-PARTY-NOTICES.txt").absolutePath,
            "--maven", maven.absolutePath,
            "--source", sourceUrl.get(),
        )
        if (debugBuild.get()) command += "--debug"
        val process = ProcessBuilder(command).directory(root).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        if (process.waitFor() != 0) throw GradleException("koru-release notices android failed:\n$output")

        root.resolve("LICENSE").copyTo(out.resolve("LICENSE.txt"), overwrite = true)
        root.resolve("NOTICE").copyTo(out.resolve("NOTICE.txt"), overwrite = true)
        out.resolve("SOURCE.txt").writeText(sourceUrl.get() + "\n")
    }

    /** Each Maven library of the classpath: coordinates, POM licenses, carried license files. */
    private fun mavenLibraries(): List<Map<String, Any?>> {
        val configuration = classpath.get()
        val modules = configuration.incoming.resolutionResult.allComponents
            .mapNotNull { it.id as? org.gradle.api.artifacts.component.ModuleComponentIdentifier }
            .distinct()
            .sortedBy { it.displayName }
        val files = mutableMapOf<String, MutableList<File>>()
        for (type in listOf("aar", "jar")) {
            configuration.incoming.artifactView {
                lenient(true)
                attributes.attribute(org.gradle.api.artifacts.type.ArtifactTypeDefinition.ARTIFACT_TYPE_ATTRIBUTE, type)
            }.artifacts.forEach { artifact ->
                val id = artifact.id.componentIdentifier
                if (id is org.gradle.api.artifacts.component.ModuleComponentIdentifier) {
                    files.getOrPut(id.displayName) { mutableListOf() } += artifact.file
                }
            }
        }
        return modules.map { module ->
            val pom = pom(module.group, module.module, module.version)
            mapOf(
                "coordinates" to "${module.group}:${module.module}:${module.version}",
                "url" to pom?.url,
                "licenses" to (pom?.licenses ?: emptyList()),
                "files" to files[module.displayName].orEmpty().flatMap(::licenseFiles).distinctBy { it["name"] },
            )
        }
    }

    private class Pom(val url: String?, val licenses: List<Map<String, String?>>)

    /** A POM's URL and licenses, the licenses taken from the nearest parent that declares any. */
    private fun pom(group: String, artifact: String, version: String, depth: Int = 0): Pom? {
        val file = runCatching {
            project.configurations.detachedConfiguration(project.dependencies.create("$group:$artifact:$version@pom"))
                .resolve().single()
        }.getOrNull() ?: return null
        val document = javax.xml.parsers.DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(file)
        val top = document.documentElement
        fun child(element: org.w3c.dom.Element, name: String): org.w3c.dom.Element? =
            (0 until element.childNodes.length).map { element.childNodes.item(it) }
                .firstOrNull { it is org.w3c.dom.Element && it.tagName == name } as org.w3c.dom.Element?
        fun text(element: org.w3c.dom.Element?, name: String) = element?.let { child(it, name) }?.textContent?.trim()
        val licenses = child(top, "licenses")?.let { list ->
            (0 until list.childNodes.length).map { list.childNodes.item(it) }
                .filterIsInstance<org.w3c.dom.Element>()
                .map { mapOf("name" to text(it, "name"), "url" to text(it, "url")) }
        }.orEmpty()
        val url = text(top, "url")
        if (licenses.isNotEmpty() || depth > 5) return Pom(url, licenses)
        val parent = child(top, "parent") ?: return Pom(url, licenses)
        val inherited = pom(text(parent, "groupId")!!, text(parent, "artifactId")!!, text(parent, "version")!!, depth + 1)
        return Pom(url ?: inherited?.url, inherited?.licenses.orEmpty())
    }

    /** The license files an artifact carries at its root or in META-INF. */
    private fun licenseFiles(file: File): List<Map<String, String>> = ZipFile(file).use { zip ->
        zip.entries().toList()
            .filter { !it.isDirectory }
            .filter { entry ->
                val name = entry.name.removePrefix("META-INF/")
                !name.contains('/') && (
                    entry.name.startsWith("third_party_licenses.") ||
                        Regex("^(LICENSE|LICENCE|NOTICE|COPYING)", RegexOption.IGNORE_CASE).containsMatchIn(name)
                    )
            }
            .map { mapOf("name" to it.name, "text" to zip.getInputStream(it).readBytes().toString(Charsets.UTF_8)) }
    }
}

/** Fails when an APK lacks a notices file, which the debug and release APKs must both carry. */
abstract class KoruNoticesInApkTask : DefaultTask() {
    @get:org.gradle.api.tasks.InputFiles
    abstract val apkDirectory: DirectoryProperty

    @get:org.gradle.api.tasks.Input
    abstract val required: ListProperty<String>

    @get:org.gradle.api.tasks.Input
    abstract val sourceUrl: Property<String>

    @org.gradle.api.tasks.TaskAction
    fun check() {
        val apks = apkDirectory.get().asFile.walk().filter { it.extension == "apk" }.toList()
        if (apks.isEmpty()) throw GradleException("no APK in ${apkDirectory.get().asFile}")
        for (apk in apks) ZipFile(apk).use { zip ->
            val missing = required.get().filter { zip.getEntry("assets/licenses/$it") == null }
            if (missing.isNotEmpty()) throw GradleException("${apk.name} carries no assets/licenses/${missing.joinToString()}")
            val notices = zip.getInputStream(zip.getEntry("assets/licenses/THIRD-PARTY-NOTICES.txt")).readBytes().toString(Charsets.UTF_8)
            for (part in listOf("Part 1", "Part 2 — Rust crates", "Part 3 — Android libraries", sourceUrl.get())) {
                if (!notices.contains(part)) throw GradleException("${apk.name}'s THIRD-PARTY-NOTICES.txt has no `$part`")
            }
        }
    }
}

fun koruSourceUrl(): String {
    val commit = koruGit("rev-parse", "HEAD")
    require(commit.matches(Regex("[0-9a-f]{40}"))) {
        "the notices name the commit this APK is built from, and git gave none (\"$commit\")"
    }
    return "https://github.com/koruworks/mono/tree/$commit"
}

androidComponents {
    onVariants { variant ->
        val suffix = variant.name.replaceFirstChar { it.uppercase() }
        // Read when a task runs, so configuring the project needs no git.
        val sourceUrl = providers.provider { koruSourceUrl() }
        val notices = tasks.register<KoruNoticesTask>("koruNotices$suffix") {
            group = "build"
            description = "Writes the ${variant.name} APK's open-source notices into its assets."
            classpath.set(variant.runtimeConfiguration)
            debugBuild.set(variant.buildType == "debug")
            this.sourceUrl.set(sourceUrl)
            repoRoot.set(koruRepoRoot)
        }
        variant.sources.assets?.addGeneratedSourceDirectory(notices, KoruNoticesTask::outputDir)
        val check = tasks.register<KoruNoticesInApkTask>("checkKoruNotices$suffix") {
            group = "verification"
            description = "Fails when the ${variant.name} APK carries no open-source notices."
            apkDirectory.set(variant.artifacts.get(com.android.build.api.artifact.SingleArtifact.APK))
            required.set(koruLicenseFiles)
            this.sourceUrl.set(sourceUrl)
        }
        tasks.configureEach { if (name == "assemble$suffix") finalizedBy(check) }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.12.01")

    implementation(composeBom)
    androidTestImplementation(composeBom)

    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.9.4")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.4")
    implementation("androidx.navigation:navigation-compose:2.9.6")
    // WorkManager, for one thing only: resuming a blob transfer Android
    // interrupted by killing this process. It is never a tick — D244 makes the
    // account poll the phone's heartbeat and this does not compete with it.
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.compose.material3:material3")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("com.google.protobuf:protobuf-kotlin-lite:4.36.2")
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    implementation("androidx.credentials:credentials:1.6.0")
    implementation("androidx.credentials:credentials-play-services-auth:1.6.0")
    implementation("com.google.android.libraries.identity.googleid:googleid:1.2.1")
    implementation("com.google.zxing:core:3.5.4")

    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20260814")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.11.0")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}
