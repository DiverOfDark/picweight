package dev.picweight.android.data.remote

import dev.picweight.android.data.remote.model.AuthConfigResponse
import dev.picweight.android.data.remote.model.DayResponse
import dev.picweight.android.data.remote.model.FoodResponse
import dev.picweight.android.data.remote.model.GroupResponse
import dev.picweight.android.data.remote.model.LogWeightRequest
import dev.picweight.android.data.remote.model.LogWeightResponse
import dev.picweight.android.data.remote.model.MeResponse
import dev.picweight.android.data.remote.model.MealAcceptedResponse
import dev.picweight.android.data.remote.model.MealResponse
import dev.picweight.android.data.remote.model.PatchMealRequest
import dev.picweight.android.data.remote.model.ReanalyzeRequest
import dev.picweight.android.data.remote.model.RecentDish
import dev.picweight.android.data.remote.model.RevisionsResponse
import dev.picweight.android.data.remote.model.TokenExchangeRequest
import dev.picweight.android.data.remote.model.TokenResponse
import dev.picweight.android.data.remote.model.UpdateProfileRequest
import dev.picweight.android.data.remote.model.UpdateProfileResponse
import dev.picweight.android.data.remote.model.WeightLogResponse
import okhttp3.MultipartBody
import okhttp3.RequestBody
import retrofit2.Call
import retrofit2.Response
import retrofit2.http.Body
import retrofit2.http.DELETE
import retrofit2.http.GET
import retrofit2.http.Multipart
import retrofit2.http.PATCH
import retrofit2.http.POST
import retrofit2.http.PUT
import retrofit2.http.Part
import retrofit2.http.PartMap
import retrofit2.http.Path
import retrofit2.http.Query

/**
 * The picweight HTTP surface (PRD §9).
 *
 * Every request and response body is one of the OpenAPI-generated models under
 * `data.remote.model` — those are regenerated from `android/openapi.json` on every
 * build, so the wire contract cannot drift from the backend's utoipa annotations.
 * Only the method shapes live here: multipart ingest and the SSE stream are not
 * things the generator expresses usefully.
 */
interface PicweightApi {

    // ---- auth -------------------------------------------------------------

    @GET("api/auth/config")
    suspend fun getAuthConfig(): AuthConfigResponse

    @POST("api/auth/token")
    suspend fun exchangeToken(@Body request: TokenExchangeRequest): TokenResponse

    // Synchronous variants for use inside OkHttp interceptors/authenticators, where
    // suspend functions can't be called.
    @POST("api/auth/token")
    fun exchangeTokenCall(@Body request: TokenExchangeRequest): Call<TokenResponse>

    @POST("api/auth/refresh")
    fun refreshTokenCall(): Call<TokenResponse>

    // ---- profile ----------------------------------------------------------

    @GET("api/v1/me")
    suspend fun getMe(): MeResponse

    @PUT("api/v1/me/profile")
    suspend fun updateProfile(@Body request: UpdateProfileRequest): UpdateProfileResponse

    // ---- capture ----------------------------------------------------------

    /**
     * Multipart ingest. `parts` carries the text fields — `client_uuid` is mandatory and
     * is the idempotency key, so a retried upload after a flaky connection can never
     * create a duplicate. Returns 202: analysis is always asynchronous.
     */
    @Multipart
    @POST("api/v1/meals")
    suspend fun createMeal(
        @PartMap parts: Map<String, @JvmSuppressWildcards RequestBody>,
        @Part photo: MultipartBody.Part?,
    ): Response<MealAcceptedResponse>

    // ---- meals ------------------------------------------------------------

    @GET("api/v1/meals/{id}")
    suspend fun getMeal(@Path("id") id: String): MealResponse

    @GET("api/v1/meals")
    suspend fun listMeals(
        @Query("from") from: String? = null,
        @Query("to") to: String? = null,
        @Query("limit") limit: Long? = null,
        @Query("offset") offset: Long? = null,
    ): List<MealResponse>

    @PATCH("api/v1/meals/{id}")
    suspend fun patchMeal(@Path("id") id: String, @Body request: PatchMealRequest): MealResponse

    @DELETE("api/v1/meals/{id}")
    suspend fun deleteMeal(@Path("id") id: String): Response<Unit>

    /** Resumes the persisted agent session and emits a new revision (PRD §5). */
    @POST("api/v1/meals/{id}/reanalyze")
    suspend fun reanalyzeMeal(
        @Path("id") id: String,
        @Body request: ReanalyzeRequest,
    ): MealAcceptedResponse

    @GET("api/v1/meals/{id}/revisions")
    suspend fun mealRevisions(@Path("id") id: String): RevisionsResponse

    // ---- sittings, days, chips -------------------------------------------

    @GET("api/v1/groups/{group_id}")
    suspend fun getGroup(@Path("group_id") groupId: String): GroupResponse

    @GET("api/v1/days/{date}")
    suspend fun getDay(
        @Path("date") date: String,
        @Query("tz_offset") tzOffset: Int? = null,
    ): DayResponse

    /** Powers the capture screen's recent-dish chips — one tap, no keyboard. */
    @GET("api/v1/dishes/recent")
    suspend fun recentDishes(@Query("limit") limit: Long? = null): List<RecentDish>

    // ---- barcode & weight -------------------------------------------------

    @POST("api/v1/barcode/{ean}")
    suspend fun resolveBarcode(@Path("ean") ean: String): FoodResponse

    @POST("api/v1/weights")
    suspend fun logWeight(@Body request: LogWeightRequest): LogWeightResponse

    @GET("api/v1/weights")
    suspend fun listWeights(@Query("limit") limit: Long? = null): List<WeightLogResponse>
}
