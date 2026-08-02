package dev.picweight.android.ui.meal

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import coil3.compose.AsyncImage
import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.local.MealItemEntity
import dev.picweight.android.ui.common.ErrorBanner
import dev.picweight.android.ui.common.MealStatusCopy
import dev.picweight.android.ui.common.RetryButton
import dev.picweight.android.ui.common.asWhole
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MealDetailScreen(
    onBack: () -> Unit,
    viewModel: MealDetailViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()

    LaunchedEffect(state.deleted) {
        if (state.deleted) onBack()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(state.meal?.dishName ?: "Meal") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = viewModel::delete) {
                        Icon(Icons.Filled.Delete, contentDescription = "Delete")
                    }
                },
            )
        },
    ) { padding ->
        val meal = state.meal
        if (meal == null) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                state.error?.let { Text(it, textAlign = TextAlign.Center, modifier = Modifier.padding(24.dp)) }
                    ?: CircularProgressIndicator()
            }
            return@Scaffold
        }

        LazyColumn(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .imePadding(),
        ) {
            viewModel.thumbnailModel(meal)?.let { image ->
                item {
                    AsyncImage(
                        model = image,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier
                            .fillMaxWidth()
                            .aspectRatio(4f / 3f)
                            .clip(RoundedCornerShape(bottomStart = 16.dp, bottomEnd = 16.dp)),
                    )
                }
            }

            item { StatusHeader(meal, state.online) }

            // A failed meal's whole screen is otherwise read-only, so the way out goes
            // directly under the header, above the item list it does not have.
            if (meal.status == LocalMealStatus.FAILED) {
                item {
                    FailedAnalysisCard(
                        reason = MealStatusCopy.detail(meal, state.online),
                        canRetry = state.canRetry,
                        retrying = state.retrying,
                        retryError = state.retryError,
                        onRetry = viewModel::retry,
                        onDismissError = viewModel::dismissRetryError,
                    )
                }
            }

            state.error?.let { item { Box(Modifier.padding(16.dp)) { ErrorBanner(it) } } }
            state.message?.let {
                item {
                    Row(
                        Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(it, style = MaterialTheme.typography.bodySmall, modifier = Modifier.weight(1f))
                        TextButton(onClick = viewModel::dismissMessage) { Text("OK") }
                    }
                }
            }

            if (state.items.isNotEmpty()) {
                item {
                    Text(
                        "Items",
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(start = 16.dp, top = 12.dp, bottom = 4.dp),
                    )
                }
                items(state.items, key = { it.position }) { item ->
                    ItemRow(
                        item = item,
                        enabled = meal.serverId != null && !state.busy,
                        onGrams = { viewModel.setItemGrams(item, it) },
                    )
                    HorizontalDivider()
                }
            }

            if (meal.serverId != null && !meal.status.isInFlight) {
                item { PortionScale(meal, enabled = !state.busy, onChange = viewModel::setPortionScale) }
            }

            item {
                ReanalyzeCard(
                    feedback = state.feedback,
                    enabled = meal.serverId != null && !state.busy,
                    onFeedback = viewModel::setFeedback,
                    onSubmit = viewModel::submitFeedback,
                )
            }

            if (meal.status == LocalMealStatus.NEEDS_REVIEW) {
                item {
                    Button(
                        onClick = viewModel::confirm,
                        enabled = !state.busy,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(16.dp),
                    ) {
                        Text("Confirm — this is what I ate")
                    }
                }
                item {
                    Text(
                        "Confirming is what makes the next order of this dish near-exact.",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp),
                    )
                }
            }

            item {
                TextButton(
                    onClick = viewModel::toggleRevisions,
                    modifier = Modifier.padding(horizontal = 8.dp),
                ) {
                    Text(if (state.showRevisions) "Hide revision history" else "Revision history")
                }
            }

            if (state.showRevisions) {
                items(state.revisions, key = { it.revision }) { revision ->
                    Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                        Text(
                            "Revision ${revision.revision} · ${revision.totals.kcal.asWhole()} kcal",
                            style = MaterialTheme.typography.titleSmall,
                        )
                        revision.userFeedback?.let {
                            Text("“$it”", style = MaterialTheme.typography.bodySmall)
                        }
                        Text(
                            buildString {
                                append(revision.model)
                                append(" · ")
                                append(if (revision.reseeded) "reseeded" else "continued")
                                append(" · ")
                                append("${revision.toolCalls} tool calls")
                            },
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    HorizontalDivider()
                }
            }

            item { Spacer(Modifier.height(32.dp)) }
        }
    }
}

@Composable
private fun StatusHeader(meal: MealEntity, online: Boolean) {
    Column(Modifier.padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = "${meal.kcal.asWhole()} kcal",
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.width(12.dp))
            if (meal.status.isInFlight) {
                CircularProgressIndicator(Modifier.height(16.dp), strokeWidth = 2.dp)
                Spacer(Modifier.width(8.dp))
            }
            Text(
                text = MealStatusCopy.badge(meal.status, online),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Spacer(Modifier.height(4.dp))
        Text(
            "${meal.proteinG.asWhole()}g protein · ${meal.fatG.asWhole()}g fat · ${meal.carbsG.asWhole()}g carbs",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // A FAILED meal's reason is rendered by FailedAnalysisCard instead, beside the
        // button that acts on it — printing it twice would just push the button off
        // screen. A QUEUED meal whose upload keeps bouncing still carries one here.
        meal.error?.takeIf { meal.status != LocalMealStatus.FAILED }?.let {
            Spacer(Modifier.height(8.dp))
            ErrorBanner(it)
        }
        meal.comment?.let {
            Spacer(Modifier.height(8.dp))
            Text("You said: $it", style = MaterialTheme.typography.bodySmall)
        }
    }
}

/**
 * The reason an analysis failed, and the way to try it again.
 *
 * The two belong in one block. "Your quota is done" is worth a tap; "the image was
 * rejected" never will be — the reason is the entire basis for the decision, so a button
 * shown without it would just be a slot machine. [canRetry] is false for the other kind
 * of failure, an upload that gave up before the server ever saw the meal: there is no
 * server-side analysis to re-run, and saying so is better than a button that 404s.
 */
@Composable
private fun FailedAnalysisCard(
    reason: String,
    canRetry: Boolean,
    retrying: Boolean,
    retryError: String?,
    onRetry: () -> Unit,
    onDismissError: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(16.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp),
    ) {
        Text(
            "Analysis failed",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.onErrorContainer,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            reason,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onErrorContainer,
        )

        if (canRetry) {
            Spacer(Modifier.height(4.dp))
            Text(
                // Says why retrying is free: the user's instinct is that a failed meal
                // has to be re-photographed, and by now the food is gone.
                "The photo is still on the server — retrying re-runs the estimate on it, " +
                    "nothing is uploaded again.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            Spacer(Modifier.height(12.dp))
            RetryButton(retrying = retrying, onRetry = onRetry)
        } else {
            Spacer(Modifier.height(4.dp))
            Text(
                "This one never reached the server, so there is no analysis to re-run.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
        }

        retryError?.let {
            Spacer(Modifier.height(12.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onErrorContainer,
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = onDismissError) { Text("OK") }
            }
        }
    }
}

@Composable
private fun ItemRow(
    item: MealItemEntity,
    enabled: Boolean,
    onGrams: (Double) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    var grams by remember(item.grams) { mutableStateOf(item.grams.toFloat()) }

    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(item.name, style = MaterialTheme.typography.bodyLarge)
                Text(
                    "${item.grams.asWhole()} g · ${item.proteinG.asWhole()}P " +
                        "${item.fatG.asWhole()}F ${item.carbsG.asWhole()}C" +
                        (item.macroSource?.let { " · $it" } ?: ""),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text("${item.kcal.asWhole()} kcal", style = MaterialTheme.typography.bodyMedium)
        }

        // One line of "why", so a wrong number is diagnosable rather than mysterious.
        item.reasoningNote?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }

        if (enabled) {
            TextButton(onClick = { expanded = !expanded }) {
                Text(if (expanded) "Done" else "Adjust grams")
            }
        }
        if (expanded) {
            Slider(
                value = grams,
                onValueChange = { grams = it },
                onValueChangeFinished = { onGrams(grams.toDouble()) },
                valueRange = 10f..(item.grams.toFloat() * 3f).coerceAtLeast(200f),
                // Snap points: nobody knows a portion to the gram, and pretending
                // otherwise makes the slider harder to use, not more accurate.
                steps = 39,
            )
            Text("${grams.roundToInt()} g", style = MaterialTheme.typography.labelMedium)
        }
    }
}

@Composable
private fun PortionScale(meal: MealEntity, enabled: Boolean, onChange: (Double) -> Unit) {
    var scale by remember(meal.portionScale) { mutableStateOf(meal.portionScale.toFloat()) }
    Column(Modifier.padding(16.dp)) {
        Text("Ate ${(scale * 100).roundToInt()}% of it", style = MaterialTheme.typography.titleSmall)
        Slider(
            value = scale,
            onValueChange = { scale = it },
            onValueChangeFinished = { onChange(scale.toDouble()) },
            valueRange = 0.1f..1.5f,
            steps = 13,
            enabled = enabled,
        )
    }
}

@Composable
private fun ReanalyzeCard(
    feedback: String,
    enabled: Boolean,
    onFeedback: (String) -> Unit,
    onSubmit: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(16.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(12.dp),
    ) {
        Text("Tell it what's wrong", style = MaterialTheme.typography.titleSmall)
        Text(
            // The value of the free-text path is that it continues the same conversation:
            // the agent still knows which container it identified and what it ruled out.
            "It picks up the same conversation, so it won't start from scratch.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = feedback,
                onValueChange = onFeedback,
                placeholder = { Text("too much rice, it was about half that") },
                modifier = Modifier.weight(1f),
                enabled = enabled,
            )
            IconButton(onClick = onSubmit, enabled = enabled && feedback.isNotBlank()) {
                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Send correction")
            }
        }
    }
}
