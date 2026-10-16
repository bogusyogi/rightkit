# The native library finds these classes and methods by name (JNI). Do not rename or strip them.
-keep class com.orthiclabs.rightkit.android.RightKitNative { *; }
-keep class com.orthiclabs.rightkit.android.RightKitKeystore { *; }
-keep class com.orthiclabs.rightkit.android.RightKitBridgeException { *; }
# UniFFI's generated Kotlin reaches the library through JNA.
-keep class com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-keep class com.orthiclabs.rightkit.android.core.** { *; }
