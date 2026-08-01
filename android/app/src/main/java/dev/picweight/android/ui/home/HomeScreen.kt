package dev.picweight.android.ui.home

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import coil3.compose.AsyncImage
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.ui.common.AuthExpiredBanner
import dev.picweight.android.ui.common.CalorieRing
import dev.picweight.android.ui.common.ErrorBanner
import dev.picweight.android.ui.common.MacroBar
import dev.picweight.android.ui.common.MacroColors
import dev.picweight.android.ui.common.MealStatusCopy
import dev.picweight.android.ui.common.RetryButton
import dev.picweight.android.ui.common.asWhole
import dev.picweight.android.ui.update.UpdateBanner
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    onCapture: () -> Unit,
    onMealClick: (String) -> Unit,
    onProfile: () -> Unit,
    onReLogin: () -> Unit,
    viewModel: HomeViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Today") },
                actions = {
                    IconButton(onClick = viewModel::refresh) {
                        Icon(Icons.Outlined.Refresh, contentDescription = "Refresh")
                    }
                    IconButton(onClick = onProfile) {
                        Icon(Icons.Outlined.Person, contentDescription = "Profile")
                    }
                },
            )
        },
        floatingActionButton = {
            // The capture affordance is deliberately the largest thing on the screen:
            // §1's whole bet is that friction per meal is what kills tracking.
            ExtendedFloatingActionButton(
                onClick = onCapture,
                icon = { Icon(Icons.Filled.PhotoCamera, contentDescription = null) },
                text = { Text("Log a meal") },
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = 96.dp),
        ) {
            if (state.authExpired) {
                item { AuthExpiredBanner(onReLogin = onReLogin) }
            }
            // Draws nothing unless the app-start check found a strictly newer build on
            // the server — including when the check failed, which must stay invisible.
            item { UpdateBanner(onOpenSettings = onProfile) }
            if (!state.hasProfile) {
                item {
                    Box(Modifier.padding(16.dp)) {
                        OnboardingPrompt(onProfile)
                    }
                }
            }

            item {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 8.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    CalorieRing(
                        consumed = state.day?.consumedKcal ?: 0.0,
                        target = state.day?.targetKcal ?: 0.0,
                    )
                    state.day?.verdict?.takeIf { it.isNotBlank() }?.let {
                        Spacer(Modifier.height(12.dp))
                        Text(
                            it,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            item {
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                    MacroBar(
                        "Protein",
                        state.day?.consumedProteinG ?: 0.0,
                        state.day?.targetProteinG ?: 0.0,
                        MacroColors.Protein,
                    )
                    Spacer(Modifier.height(10.dp))
                    MacroBar(
                        "Fat",
                        state.day?.consumedFatG ?: 0.0,
                        state.day?.targetFatG ?: 0.0,
                        MacroColors.Fat,
                    )
                    Spacer(Modifier.height(10.dp))
                    MacroBar(
                        "Carbs",
                        state.day?.consumedCarbsG ?: 0.0,
                        state.day?.targetCarbsG ?: 0.0,
                        MacroColors.Carbs,
                    )
                }
            }

            state.error?.let {
                item { Box(Modifier.padding(16.dp)) { ErrorBanner(it) } }
            }

            // A refused retry gets its own line rather than sharing the refresh banner:
            // it is a different request, and the reason ("no longer failed", "409") only
            // makes sense next to the button that caused it.
            state.retryError?.let {
                item {
                    Row(
                        Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(Modifier.weight(1f)) { ErrorBanner(it) }
                        TextButton(onClick = viewModel::dismissRetryError) { Text("OK") }
                    }
                }
            }

            item { HorizontalDivider() }

            if (state.rows.isEmpty()) {
                item {
                    Text(
                        text = "Nothing logged yet today.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(24.dp),
                    )
                }
            }

            items(state.rows, key = { row ->
                when (row) {
                    is DayRow.Single -> "meal:${row.meal.clientUuid}"
                    is DayRow.Sitting -> "sitting:${row.groupId}"
                }
            }) { row ->
                when (row) {
                    is DayRow.Single -> MealRow(
                        meal = row.meal,
                        thumbnailModel = viewModel.thumbnailModel(row.meal),
                        online = state.online,
                        canRetry = state.canRetry(row.meal),
                        retrying = state.isRetrying(row.meal),
                        onRetry = { viewModel.retry(row.meal) },
                        onClick = { onMealClick(row.meal.clientUuid) },
                    )

                    is DayRow.Sitting -> SittingRow(
                        row = row,
                        thumbnailModel = { viewModel.thumbnailModel(it) },
                        online = state.online,
                        canRetry = state::canRetry,
                        retrying = state::isRetrying,
                        onRetry = viewModel::retry,
                        onMealClick = onMealClick,
                    )
                }
                HorizontalDivider()
            }
        }
    }
}

@Composable
private fun OnboardingPrompt(onProfile: () -> Unit) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.primaryContainer)
            .clickable(onClick = onProfile)
            .padding(16.dp),
    ) {
        Text(
            "Set your body data",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
        )
        Text(
            "Height, weight and goal give you a real target instead of a running total.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onPrimaryContainer,
        )
    }
}

/**
 * One meal in the day list.
 *
 * Informational in every state but one. A meal the server failed to analyse is the single
 * case where the row is something to *act on*, so it carries a real button and the reason
 * beside it — the reason is what decides whether acting is worth anything at all
 * ("your quota is done": yes; "the image was rejected": no).
 */
@Composable
private fun MealRow(
    meal: MealEntity,
    thumbnailModel: Any?,
    online: Boolean,
    canRetry: Boolean,
    retrying: Boolean,
    onRetry: () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier
                    .size(52.dp)
                    .clip(RoundedCornerShape(10.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant),
                contentAlignment = Alignment.Center,
            ) {
                if (thumbnailModel != null) {
                    AsyncImage(
                        model = thumbnailModel,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize(),
                    )
                } else {
                    Icon(
                        Icons.Filled.PhotoCamera,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Spacer(Modifier.size(12.dp))

            Column(Modifier.weight(1f)) {
                Text(
                    text = meal.dishName ?: MealStatusCopy.title(meal, online),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Medium,
                )
                val subtitle = when {
                    meal.status == LocalMealStatus.FAILED || meal.status.isInFlight ->
                        MealStatusCopy.detail(meal, online)

                    else -> "${localTime(meal)} · ${meal.proteinG.asWhole()}P " +
                        "${meal.fatG.asWhole()}F ${meal.carbsG.asWhole()}C"
                }
                // The failure reason, in full. It is the only input the user has to the
                // decision the Retry button asks them to make, so it is not truncated
                // here the way a dish list is.
                Text(
                    text = subtitle,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (MealStatusCopy.isProblem(meal)) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            if (!meal.status.isInFlight && meal.status != LocalMealStatus.FAILED) {
                Spacer(Modifier.size(8.dp))
                Text(
                    text = "${meal.kcal.asWhole()} kcal",
                    style = MaterialTheme.typography.titleSmall,
                )
            }
        }

        // Its own line, under the reason it belongs to: a filled button squeezed against
        // a wrapping error sentence is neither readable nor comfortably tappable.
        if (canRetry) {
            Spacer(Modifier.height(8.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.End,
            ) {
                RetryButton(retrying = retrying, onRetry = onRetry)
            }
        }
    }
}

@Composable
private fun SittingRow(
    row: DayRow.Sitting,
    thumbnailModel: (MealEntity) -> Any?,
    online: Boolean,
    canRetry: (MealEntity) -> Boolean,
    retrying: (MealEntity) -> Boolean,
    onRetry: (MealEntity) -> Unit,
    onMealClick: (String) -> Unit,
) {
    Column(Modifier.padding(vertical = 4.dp)) {
        Row(
            Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    text = "${row.meals.size} dishes",
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Medium,
                )
                Text(
                    text = if (row.hasFailures) {
                        "One dish in this sitting failed — the rest are counted."
                    } else {
                        row.meals.mapNotNull { it.dishName }.joinToString(" · ").ifBlank { "Analysing…" }
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = if (row.hasFailures) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                )
            }
            Text("${row.kcal.asWhole()} kcal", style = MaterialTheme.typography.titleSmall)
        }
        // Expanded per-dish rows: a correction — or a retry — has to reach one dish, not
        // the sitting. Five photos are five independent agent loops (PRD §5), so exactly
        // the one that ran out of quota is the one that gets asked again.
        row.meals.forEach { meal ->
            Row(Modifier.padding(start = 16.dp)) {
                MealRow(
                    meal = meal,
                    thumbnailModel = thumbnailModel(meal),
                    online = online,
                    canRetry = canRetry(meal),
                    retrying = retrying(meal),
                    onRetry = { onRetry(meal) },
                    onClick = { onMealClick(meal.clientUuid) },
                )
            }
        }
    }
}

private fun localTime(meal: MealEntity): String =
    Instant.ofEpochMilli(meal.eatenAt)
        .atZone(ZoneId.systemDefault())
        .toLocalTime()
        .format(TIME)
