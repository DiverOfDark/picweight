package dev.picweight.android.ui.auth

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import dev.picweight.android.ui.common.ErrorBanner

@Composable
fun LoginScreen(
    onLoginSuccess: () -> Unit,
    viewModel: LoginViewModel = hiltViewModel(),
) {
    val uiState by viewModel.uiState.collectAsState()
    val authIntent by viewModel.authIntent.collectAsState()
    val context = LocalContext.current

    val authLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        viewModel.handleAuthResult(result.data)
    }

    LaunchedEffect(authIntent) {
        authIntent?.let {
            authLauncher.launch(it)
            viewModel.clearAuthIntent()
        }
    }

    LaunchedEffect(uiState.isLoggedIn) {
        if (uiState.isLoggedIn) onLoginSuccess()
    }

    Scaffold { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("picweight", style = MaterialTheme.typography.displaySmall, textAlign = TextAlign.Center)
            Spacer(Modifier.height(8.dp))
            Text(
                "Photograph what you eat.",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            Spacer(Modifier.height(40.dp))

            OutlinedTextField(
                value = uiState.serverUrl,
                onValueChange = viewModel::updateServerUrl,
                label = { Text("Server URL") },
                placeholder = { Text("https://picweight.example.com") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(Modifier.height(8.dp))

            OutlinedButton(
                onClick = viewModel::fetchAuthConfig,
                modifier = Modifier.fillMaxWidth(),
                enabled = uiState.serverUrl.isNotBlank() && !uiState.isFetchingConfig,
            ) {
                if (uiState.isFetchingConfig) {
                    CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.height(8.dp))
                    Text("Detecting…")
                } else {
                    Text("Auto-detect auth config")
                }
            }

            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = uiState.oidcIssuer,
                onValueChange = viewModel::updateOidcIssuer,
                label = { Text("OIDC issuer") },
                placeholder = { Text("https://auth.example.com") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = uiState.oidcClientId,
                onValueChange = viewModel::updateOidcClientId,
                label = { Text("Native client ID") },
                supportingText = { Text("The public/native client — no secret on the phone.") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(Modifier.height(24.dp))

            uiState.error?.let {
                ErrorBanner(it)
                Spacer(Modifier.height(16.dp))
            }
            if (uiState.error == null) {
                uiState.info?.let {
                    Text(
                        text = it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.primary,
                        textAlign = TextAlign.Center,
                    )
                    Spacer(Modifier.height(16.dp))
                }
            }

            Button(
                onClick = { viewModel.startLogin(context) },
                modifier = Modifier.fillMaxWidth(),
                enabled = !uiState.isLoading && uiState.serverUrl.isNotBlank(),
            ) {
                if (uiState.isLoading) {
                    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                } else {
                    Text("Sign in with SSO")
                }
            }
        }
    }
}
