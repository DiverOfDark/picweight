package dev.picweight.android.ui.update

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import dev.picweight.android.update.InstallState
import dev.picweight.android.update.UpdateState
import java.util.Locale

/**
 * The "Check for updates" block on the profile screen.
 *
 * Always shows what is running, because that is the fact a self-hosting user needs
 * when something looks wrong; shows what is available only when there genuinely is
 * something newer.
 */
@Composable
fun UpdateSection(
    modifier: Modifier = Modifier,
    viewModel: UpdateViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()
    val context = LocalContext.current

    Column(modifier = modifier.fillMaxWidth()) {
        Text("App version", style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.Medium)
        Spacer(Modifier.height(4.dp))
        Text(
            "Running ${state.runningVersionName} (build ${state.runningVersionCode})",
            style = MaterialTheme.typography.bodyMedium,
        )

        Spacer(Modifier.height(8.dp))

        when (val update = state.update) {
            is UpdateState.Available -> AvailableRow(
                available = update,
                install = state.install,
                onInstall = { viewModel.install(update) },
                onDismiss = viewModel::dismiss,
                onGrantPermission = { context.startActivity(viewModel.permissionSettingsIntent()) },
            )

            UpdateState.UpToDate -> Muted("This is the newest build the server has.")

            UpdateState.Checking -> Muted("Checking…")

            // Never checked — either the app just started or there is no server yet.
            // Says nothing rather than guessing.
            UpdateState.Unknown -> Unit

            is UpdateState.Failed -> Muted(UpdateCopy.forFailure(update.failure))
        }

        Spacer(Modifier.height(8.dp))
        OutlinedButton(
            onClick = viewModel::check,
            enabled = !state.busy,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Check for updates")
        }
    }
}

@Composable
private fun AvailableRow(
    available: UpdateState.Available,
    install: InstallState,
    onInstall: () -> Unit,
    onDismiss: () -> Unit,
    onGrantPermission: () -> Unit,
) {
    Column(Modifier.fillMaxWidth()) {
        Text(
            "${available.versionName} (build ${available.versionCode}) is available — " +
                formatSize(available.sizeBytes),
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.Medium,
        )
        Spacer(Modifier.height(8.dp))

        when (install) {
            InstallState.Idle -> Button(onClick = onInstall, modifier = Modifier.fillMaxWidth()) {
                Text("Download and install")
            }

            is InstallState.Downloading -> Column(Modifier.fillMaxWidth()) {
                LinearProgressIndicator(
                    progress = { install.fraction },
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(4.dp))
                Muted("Downloading ${formatSize(install.bytesRead)} of ${formatSize(install.totalBytes)}")
            }

            InstallState.Verifying ->
                // Named, not hidden behind a spinner: this is the step that makes
                // installing a network download defensible, and the user should see
                // that it happens.
                Muted("Checking the download's checksum and signing certificate…")

            InstallState.AwaitingConfirmation ->
                Muted("Waiting for Android's install confirmation.")

            InstallState.Installed -> Muted("Installed. Restart picweight to use it.")

            InstallState.Declined -> Column {
                Muted("Install cancelled.")
                TextButton(onClick = onDismiss) { Text("Try again") }
            }

            // A verification failure. Rendered in the error colour and never auto-retried:
            // the artefact is not what it said it was, and downloading it again is unlikely
            // to change that.
            is InstallState.Refused -> Column {
                Loud(install.reason)
                TextButton(onClick = onDismiss) { Text("Dismiss") }
            }

            is InstallState.Failed -> Column {
                Muted(install.reason)
                TextButton(onClick = onDismiss) { Text("Try again") }
            }

            InstallState.PermissionRequired -> Column {
                Muted(
                    "Android needs permission to install apps from picweight. " +
                        "This is the switch that lets the app update itself."
                )
                TextButton(onClick = onGrantPermission) { Text("Open settings") }
            }
        }
    }
}

@Composable
private fun Muted(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun Loud(text: String) {
    Text(
        text,
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onErrorContainer,
    )
}

/**
 * A one-line prompt for the home screen.
 *
 * Renders nothing at all unless an update is genuinely available, so the app-start
 * check is invisible when there is nothing to say — including when it failed.
 */
@Composable
fun UpdateBanner(
    onOpenSettings: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: UpdateViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()
    val available = state.update as? UpdateState.Available ?: return

    Row(
        modifier = modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .clickable(onClick = onOpenSettings)
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(
            "picweight ${available.versionName} is available on your server",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
            modifier = Modifier.weight(1f),
        )
        Text(
            "Update",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
        )
    }
}

/** MB to one decimal — the only unit an APK is ever worth quoting in. */
private fun formatSize(bytes: Long): String =
    String.format(Locale.getDefault(), "%.1f MB", bytes / 1_048_576.0)
