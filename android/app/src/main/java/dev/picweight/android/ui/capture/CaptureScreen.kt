package dev.picweight.android.ui.capture

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.view.CameraController
import androidx.camera.view.LifecycleCameraController
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Camera
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Done
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel
import androidx.lifecycle.compose.LocalLifecycleOwner
import coil3.compose.AsyncImage
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.ui.common.ErrorBanner
import dev.picweight.android.ui.common.asWhole
import java.io.File
import java.util.concurrent.Executors

/**
 * The capture screen (PRD §5, §7).
 *
 * Everything on it is arranged around one number: the median capture→logged time has
 * to stay under ten seconds (G1). So the shutter is the biggest control, the comment
 * field is collapsed until asked for, and the two zero-keyboard inputs — recent-dish
 * chips and an on-device barcode read — sit directly above the shutter.
 */
@Composable
fun CaptureScreen(
    onDone: () -> Unit,
    onMealClick: (String) -> Unit,
    viewModel: CaptureViewModel = hiltViewModel(),
) {
    val state by viewModel.uiState.collectAsState()
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current

    var hasCameraPermission by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted -> hasCameraPermission = granted }

    LaunchedEffect(Unit) {
        if (!hasCameraPermission) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    LaunchedEffect(state.finished) {
        if (state.finished) onDone()
    }

    val executor = remember { Executors.newSingleThreadExecutor() }
    val analyzer = remember { BarcodeAnalyzer { ean -> viewModel.onBarcodeScanned(ean) } }
    val controller = remember { LifecycleCameraController(context) }

    DisposableEffect(hasCameraPermission, lifecycleOwner) {
        if (hasCameraPermission) {
            controller.setEnabledUseCases(
                CameraController.IMAGE_CAPTURE or CameraController.IMAGE_ANALYSIS
            )
            controller.setImageAnalysisAnalyzer(executor, analyzer)
            controller.bindToLifecycle(lifecycleOwner)
        }
        onDispose {
            controller.clearImageAnalysisAnalyzer()
            controller.unbind()
        }
    }

    DisposableEffect(Unit) {
        onDispose {
            analyzer.close()
            executor.shutdown()
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        if (hasCameraPermission) {
            AndroidView(
                factory = { ctx ->
                    PreviewView(ctx).apply {
                        scaleType = PreviewView.ScaleType.FILL_CENTER
                        this.controller = controller
                    }
                },
                modifier = Modifier.fillMaxSize(),
            )
        } else {
            Column(
                Modifier
                    .fillMaxSize()
                    .padding(32.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(
                    "picweight needs the camera to log a meal from a photo.",
                    color = Color.White,
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyLarge,
                )
                Spacer(Modifier.height(16.dp))
                Button(onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) }) {
                    Text("Grant camera access")
                }
                Spacer(Modifier.height(8.dp))
                Text(
                    "Or log it by name below — the agent works without a photo too.",
                    color = Color.White.copy(alpha = 0.7f),
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }

        IconButton(
            onClick = viewModel::finish,
            modifier = Modifier
                .align(Alignment.TopStart)
                .padding(top = 40.dp, start = 8.dp),
        ) {
            Icon(Icons.Filled.Close, contentDescription = "Close", tint = Color.White)
        }

        if (state.shots.isNotEmpty()) {
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surface.copy(alpha = 0.85f),
                modifier = Modifier
                    .align(Alignment.TopEnd)
                    .padding(top = 40.dp, end = 12.dp),
            ) {
                Text(
                    text = "${state.shots.size} in this sitting",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
                )
            }
        }

        CapturePanel(
            state = state,
            onChip = viewModel::pickChip,
            onDishName = viewModel::setDishName,
            onClearDishName = viewModel::clearDishName,
            onToggleComment = viewModel::toggleComment,
            onComment = viewModel::setComment,
            onDismissBarcode = viewModel::dismissBarcode,
            onDismissError = viewModel::dismissError,
            onShutter = {
                if (!hasCameraPermission) {
                    permissionLauncher.launch(Manifest.permission.CAMERA)
                } else {
                    val file = viewModel.newCaptureFile()
                    controller.takePicture(
                        ImageCapture.OutputFileOptions.Builder(file).build(),
                        executor,
                        object : ImageCapture.OnImageSavedCallback {
                            override fun onImageSaved(results: ImageCapture.OutputFileResults) {
                                viewModel.onPhotoCaptured(file)
                            }

                            override fun onError(exception: ImageCaptureException) {
                                file.delete()
                                viewModel.onCaptureFailed(
                                    exception.message ?: "The camera couldn't save that shot"
                                )
                            }
                        },
                    )
                }
            },
            onManual = viewModel::logWithoutPhoto,
            onFinish = viewModel::finish,
            onShotClick = onMealClick,
            modifier = Modifier.align(Alignment.BottomCenter),
        )
    }
}

@Composable
private fun CapturePanel(
    state: CaptureUiState,
    onChip: (dev.picweight.android.data.local.RecentDishEntity) -> Unit,
    onDishName: (String) -> Unit,
    onClearDishName: () -> Unit,
    onToggleComment: () -> Unit,
    onComment: (String) -> Unit,
    onDismissBarcode: () -> Unit,
    onDismissError: () -> Unit,
    onShutter: () -> Unit,
    onManual: () -> Unit,
    onFinish: () -> Unit,
    onShotClick: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.fillMaxWidth(),
        shape = RoundedCornerShape(topStart = 20.dp, topEnd = 20.dp),
        tonalElevation = 3.dp,
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {

            state.error?.let {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.weight(1f)) { ErrorBanner(it) }
                    IconButton(onClick = onDismissError) {
                        Icon(Icons.Filled.Close, contentDescription = "Dismiss")
                    }
                }
                Spacer(Modifier.height(8.dp))
            }

            // The sitting so far. Each thumbnail is already a real meal with its own
            // agent loop, so tapping one opens it.
            if (state.shots.isNotEmpty()) {
                LazyRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    contentPadding = PaddingValues(vertical = 4.dp),
                ) {
                    items(state.shots, key = { it.clientUuid }) { shot ->
                        ShotThumbnail(shot) { onShotClick(shot.clientUuid) }
                    }
                }
                Spacer(Modifier.height(8.dp))
            }

            state.barcode?.let { ean ->
                InputChip(
                    selected = true,
                    onClick = onDismissBarcode,
                    label = { Text(state.barcodeProduct ?: "Barcode $ean") },
                    trailingIcon = { Icon(Icons.Filled.Close, contentDescription = "Clear barcode") },
                )
                Spacer(Modifier.height(8.dp))
            }

            // Recent dishes: one tap, no keyboard, and more accurate than the model's
            // visual read because it is something the user already vetted (§1).
            if (state.chips.isNotEmpty()) {
                LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(state.chips, key = { it.dishNameNormalized }) { dish ->
                        AssistChip(
                            onClick = { onChip(dish) },
                            label = {
                                Text(
                                    "${dish.dishName} · ${dish.kcal.asWhole()} kcal",
                                    maxLines = 1,
                                )
                            },
                        )
                    }
                }
                Spacer(Modifier.height(8.dp))
            }

            OutlinedTextField(
                value = state.dishName,
                onValueChange = onDishName,
                label = { Text("Dish (optional)") },
                singleLine = true,
                trailingIcon = {
                    if (state.dishName.isNotEmpty()) {
                        IconButton(onClick = onClearDishName) {
                            Icon(Icons.Filled.Close, contentDescription = "Clear")
                        }
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            )

            // Skippable in one gesture: it is a text button until it is asked for, and
            // nothing on this screen ever waits for it.
            if (state.commentOpen) {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = state.comment,
                    onValueChange = onComment,
                    label = { Text("Anything the photo won't show") },
                    placeholder = { Text("fried in butter · half portion · 0.33 can") },
                    modifier = Modifier.fillMaxWidth(),
                )
                TextButton(onClick = onToggleComment) { Text("Skip") }
            } else {
                TextButton(onClick = onToggleComment) { Text("Add a comment") }
            }

            Spacer(Modifier.height(4.dp))

            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                TextButton(onClick = onManual, enabled = !state.busy) {
                    Text("Log without photo")
                }

                FloatingActionButton(
                    onClick = onShutter,
                    modifier = Modifier.size(76.dp),
                ) {
                    if (state.busy) {
                        CircularProgressIndicator(Modifier.size(28.dp), strokeWidth = 3.dp)
                    } else {
                        Icon(
                            imageVector = if (state.shots.isEmpty()) Icons.Filled.Camera else Icons.Filled.Add,
                            contentDescription = if (state.shots.isEmpty()) "Take photo" else "Add another dish",
                            modifier = Modifier.size(34.dp),
                        )
                    }
                }

                if (state.shots.isEmpty()) {
                    Spacer(Modifier.width(96.dp))
                } else {
                    FilledTonalButton(onClick = onFinish, enabled = !state.busy) {
                        Icon(Icons.Filled.Done, contentDescription = null)
                        Spacer(Modifier.width(6.dp))
                        Text("Done")
                    }
                }
            }

            if (state.shots.isNotEmpty()) {
                Text(
                    text = "Each dish is analysed on its own; you'll get one notification for the sitting.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }
        }
    }
}

@Composable
private fun ShotThumbnail(shot: MealEntity, onClick: () -> Unit) {
    Box(
        Modifier
            .size(64.dp)
            .clip(RoundedCornerShape(10.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable(onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        // The local file is gone the moment the upload lands; fall back to the server's
        // thumbnail is not needed here because the strip only shows the live sitting.
        val local = shot.photoPath?.let(::File)?.takeIf { it.isFile }
        if (local != null) {
            AsyncImage(
                model = local,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        } else {
            Icon(Icons.Filled.Camera, contentDescription = null)
        }
    }
}
