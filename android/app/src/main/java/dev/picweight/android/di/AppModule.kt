package dev.picweight.android.di

import com.fasterxml.jackson.annotation.JsonInclude
import com.fasterxml.jackson.databind.DeserializationFeature
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.SerializationFeature
import com.fasterxml.jackson.databind.json.JsonMapper
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule
import com.fasterxml.jackson.module.kotlin.KotlinModule
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import dev.picweight.android.data.remote.AuthInterceptor
import dev.picweight.android.data.remote.BaseUrlInterceptor
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.PicweightAuthenticator
import okhttp3.OkHttpClient
import okhttp3.logging.HttpLoggingInterceptor
import retrofit2.Retrofit
import retrofit2.converter.jackson.JacksonConverterFactory
import java.util.concurrent.TimeUnit
import javax.inject.Singleton

@Module
@InstallIn(SingletonComponent::class)
object AppModule {

    @Provides
    @Singleton
    fun provideObjectMapper(): ObjectMapper = JsonMapper.builder()
        .addModule(KotlinModule.Builder().build())
        // The generated models use java.time; the wire format is RFC 3339, not epoch.
        .addModule(JavaTimeModule())
        .disable(SerializationFeature.WRITE_DATES_AS_TIMESTAMPS)
        // The spec is generated from the backend's own types, but tolerating unknown
        // fields is what lets an older APK keep working against a newer server.
        .disable(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES)
        // PATCH bodies are sparse: an omitted field means "leave it alone".
        .defaultPropertyInclusion(JsonInclude.Value.construct(JsonInclude.Include.NON_NULL, JsonInclude.Include.ALWAYS))
        .build()

    @Provides
    @Singleton
    fun provideOkHttpClient(
        baseUrlInterceptor: BaseUrlInterceptor,
        authInterceptor: AuthInterceptor,
        authenticator: PicweightAuthenticator,
    ): OkHttpClient {
        return OkHttpClient.Builder()
            .authenticator(authenticator)
            .addInterceptor(baseUrlInterceptor)
            .addInterceptor(authInterceptor)
            .addInterceptor(HttpLoggingInterceptor().apply {
                level = HttpLoggingInterceptor.Level.BASIC
            })
            .connectTimeout(30, TimeUnit.SECONDS)
            // The agent loop is bounded at ~25s server-side (PRD §5); a 60s read timeout
            // covers it with room to spare without hiding a genuinely stuck request.
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(120, TimeUnit.SECONDS) // photo uploads on a bad connection
            .build()
    }

    @Provides
    @Singleton
    fun provideRetrofit(
        okHttpClient: OkHttpClient,
        objectMapper: ObjectMapper,
    ): Retrofit {
        return Retrofit.Builder()
            .baseUrl(BaseUrlInterceptor.PLACEHOLDER_BASE_URL)
            .client(okHttpClient)
            .addConverterFactory(JacksonConverterFactory.create(objectMapper))
            .build()
    }

    @Provides
    @Singleton
    fun providePicweightApi(retrofit: Retrofit): PicweightApi =
        retrofit.create(PicweightApi::class.java)
}
