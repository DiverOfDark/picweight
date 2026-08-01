# Retrofit
-keepattributes Signature, InnerClasses, EnclosingMethod
-keepattributes RuntimeVisibleAnnotations, RuntimeVisibleParameterAnnotations
-keepclassmembers,allowshrinking,allowobfuscation interface * {
    @retrofit2.http.* <methods>;
}

# The OpenAPI-generated wire models are populated purely by Jackson reflection, so
# nothing in the app references their members directly and R8 would strip them.
-keepclassmembers class dev.picweight.android.data.remote.** {
    *;
}

# Jackson TypeReference (used by reified readValue<T>) resolves its generic supertype
# reflectively; keep subclasses so R8 retains their Signature attribute (-keepattributes
# only applies to classes matched by a keep rule).
-keep,allowobfuscation,allowshrinking class * extends com.fasterxml.jackson.core.type.TypeReference

# Room
-keep class * extends androidx.room.RoomDatabase
-dontwarn androidx.room.paging.**

# AppAuth
-keep class net.openid.appauth.** { *; }

# The generated models carry javax.annotation.* markers that are compileOnly.
-dontwarn javax.annotation.**

# ML Kit ships optional model backends it resolves at runtime.
-dontwarn com.google.mlkit.**
