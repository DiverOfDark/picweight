package dev.picweight.android.ui.profile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.KeyboardOptions
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import dev.picweight.android.data.remote.model.GoalType
import dev.picweight.android.data.remote.model.Sex
import dev.picweight.android.ui.common.ErrorBanner
import dev.picweight.android.ui.common.asWhole
import dev.picweight.android.ui.update.UpdateSection

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileScreen(
    onBack: () -> Unit,
    onLoggedOut: () -> Unit,
    viewModel: ProfileViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()

    LaunchedEffect(state.loggedOut) {
        if (state.loggedOut) onLoggedOut()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("You") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
        ) {
            state.me?.user?.let { user ->
                Text(
                    user.displayName ?: user.email ?: "Signed in",
                    style = MaterialTheme.typography.titleMedium,
                )
                state.serverUrl?.let {
                    Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                Spacer(Modifier.height(16.dp))
            }

            state.error?.let {
                ErrorBanner(it)
                Spacer(Modifier.height(12.dp))
            }

            state.me?.profile?.let { profile ->
                Text("Targets", style = MaterialTheme.typography.titleSmall)
                Text(
                    buildString {
                        append(profile.targetKcal?.asWhole() ?: "—")
                        append(" kcal · ")
                        append(profile.targetProteinG?.asWhole() ?: "—")
                        append("g protein · ")
                        append(profile.targetFatG?.asWhole() ?: "—")
                        append("g fat · ")
                        append(profile.targetCarbsG?.asWhole() ?: "—")
                        append("g carbs")
                    },
                    style = MaterialTheme.typography.bodyMedium,
                )
                Spacer(Modifier.height(16.dp))
                HorizontalDivider()
                Spacer(Modifier.height(16.dp))
            }

            Text("Body data", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Medium)
            Text(
                "The target is arithmetic, not a guess: Mifflin-St Jeor over these numbers.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(12.dp))

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Sex.values().forEach { option ->
                    FilterChip(
                        selected = state.sex == option,
                        onClick = { viewModel.setSex(option) },
                        label = { Text(option.value) },
                    )
                }
            }

            Spacer(Modifier.height(12.dp))

            OutlinedTextField(
                value = state.birthDate,
                onValueChange = viewModel::setBirthDate,
                label = { Text("Date of birth") },
                placeholder = { Text("1990-04-17") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(8.dp))
            NumberField("Height (cm)", state.heightCm, viewModel::setHeight)
            Spacer(Modifier.height(8.dp))
            NumberField("Current weight (kg)", state.currentWeightKg, viewModel::setCurrentWeight)
            Spacer(Modifier.height(8.dp))
            NumberField("Target weight (kg)", state.targetWeightKg, viewModel::setTargetWeight)

            Spacer(Modifier.height(16.dp))
            Text(
                "Activity ×${String.format("%.2f", state.activityFactor)}",
                style = MaterialTheme.typography.labelLarge,
            )
            Slider(
                value = state.activityFactor,
                onValueChange = viewModel::setActivityFactor,
                valueRange = 1.2f..1.9f,
                steps = 13,
            )
            Text(
                "1.2 desk-bound · 1.55 a few sessions a week · 1.9 physical job or daily training",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Spacer(Modifier.height(16.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                GoalType.values().forEach { option ->
                    FilterChip(
                        selected = state.goalType == option,
                        onClick = { viewModel.setGoalType(option) },
                        label = { Text(option.value) },
                    )
                }
            }
            Spacer(Modifier.height(8.dp))
            NumberField("Rate (kg per week)", state.rateKgPerWeek, viewModel::setRate)

            Spacer(Modifier.height(8.dp))
            OutlinedTextField(
                value = state.timezone,
                onValueChange = viewModel::setTimezone,
                label = { Text("Timezone") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            if (state.warnings.isNotEmpty()) {
                Spacer(Modifier.height(12.dp))
                state.warnings.forEach {
                    // Warned about, never silently accepted.
                    Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
            }

            Spacer(Modifier.height(16.dp))
            Button(
                onClick = viewModel::save,
                enabled = !state.busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (state.saved) "Saved" else "Save and recompute targets")
            }

            Spacer(Modifier.height(24.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            Text("Log a weight", style = MaterialTheme.typography.titleSmall)
            Row(Modifier.fillMaxWidth(), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                OutlinedTextField(
                    value = state.weightEntry,
                    onValueChange = viewModel::setWeightEntry,
                    label = { Text("kg") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(8.dp))
                Button(onClick = viewModel::logWeight, enabled = !state.busy) { Text("Log") }
            }

            Spacer(Modifier.height(24.dp))
            HorizontalDivider()
            Spacer(Modifier.height(16.dp))

            // Self-hosted distribution: there is no store to push a build, so the app
            // asks its own server whether the image it is talking to ships a newer APK.
            UpdateSection()

            Spacer(Modifier.height(32.dp))
            OutlinedButton(onClick = viewModel::logout, modifier = Modifier.fillMaxWidth()) {
                Text("Sign out and clear this device")
            }
            state.version?.let {
                Spacer(Modifier.height(12.dp))
                Text(
                    "backend $it",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(32.dp))
        }
    }
}

@Composable
private fun NumberField(label: String, value: String, onValueChange: (String) -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
        modifier = Modifier.fillMaxWidth(),
    )
}
