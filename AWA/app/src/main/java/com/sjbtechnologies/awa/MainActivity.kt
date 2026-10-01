package com.sjbtechnologies.awa

import android.Manifest
import android.app.Activity
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Cameraswitch
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.FlashOff
import androidx.compose.material.icons.filled.FlashOn
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.PowerSettingsNew
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalInspectionMode
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Popup
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.compose.viewModel
import com.sjbtechnologies.awa.ui.theme.AWATheme
import com.sjbtechnologies.awa.ui.components.Preview
import com.sjbtechnologies.awa.viewModel.CameraViewModel
import java.net.Inet4Address
import java.net.NetworkInterface

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
            )
        }
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR
        WindowCompat.setDecorFitsSystemWindows(window, false)
        val controller = WindowInsetsControllerCompat(window, window.decorView)
        controller.hide(WindowInsetsCompat.Type.statusBars())
        controller.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE

        enableEdgeToEdge()
        setContent {
            AWATheme {
                Scaffold(modifier = Modifier.fillMaxSize()) { _ ->
                    CameraScreen()
                }
            }
        }
    }

    private fun activityViewModel(): CameraViewModel =
        ViewModelProvider(this)[CameraViewModel::class.java]

    override fun onPause() {
        super.onPause()
        // Release ONLY our camera session when backgrounded — other apps are untouched,
        // and we stop squatting on (or wedging) the shared HAL while invisible.
        try {
            activityViewModel().pauseCamera()
        } catch (e: Exception) {
            Log.e("AWA", "pauseCamera failed", e)
        }
    }

    override fun onResume() {
        super.onResume()
        try {
            activityViewModel().resumeCamera()
        } catch (e: Exception) {
            Log.e("AWA", "resumeCamera failed", e)
        }
    }
}

@Composable
fun CameraScreen(camView: CameraViewModel = viewModel()) {
    // POST_NOTIFICATIONS is needed on API 33+ for the screen-off streaming foreground
    // service notification. Without it the service cannot promote to foreground.
    val hasPermission by checkPermissions(
        Manifest.permission.CAMERA,
        Manifest.permission.POST_NOTIFICATIONS
    )

    if (hasPermission) {
        CameraContent(camView)
    } else {
        Box(
            modifier = Modifier.fillMaxSize(),
            contentAlignment = Alignment.Center
        ) {
            Text("Camera permission is required to stream.")
        }
    }
}

@Composable
private fun CameraContent(camView: CameraViewModel) {
    val isServerRunning by camView.isServerRunning
    var isSettingsOpen by remember { mutableStateOf(false) }

    val context = LocalContext.current

    var showExposureSlider by remember { mutableStateOf(false) }

    val settings by camView.settings
    val exposureIndex = settings.exposureIndex
    val exposureRange = settings.exposureRange
    val focusMode = settings.focusMode
    val focusDistance = settings.focusDistance
    var showFocusSlider by remember { mutableStateOf(false) }
    val ipAddress by remember { mutableStateOf(getLocalIpAddress()) }
    val lastInteractionMs by camView.lastInteractionMs
    var screenDimmed by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        camView.initialize(context)
    }

    // Battery saver: dim the backlight to near-off after 15s without interaction
    // while streaming (display is the #1 drain; streaming needs no screen).
    // Any tap wakes it back via notifyScreenTapped -> interaction tick.
    LaunchedEffect(isServerRunning, lastInteractionMs) {
        val activity = context as? Activity
        if (activity == null) return@LaunchedEffect
        while (true) {
            val idleMs = android.os.SystemClock.uptimeMillis() - camView.lastInteractionMs.value
            val shouldDim = isServerRunning && idleMs > 15_000
            if (shouldDim != screenDimmed) {
                screenDimmed = shouldDim
                try {
                    activity.runOnUiThread {
                        val lp = activity.window.attributes
                        lp.screenBrightness = if (shouldDim) 0.02f else -1f
                        activity.window.attributes = lp
                    }
                    Log.d("AWA", if (shouldDim) "Screen dimmed for battery saving" else "Screen brightness restored")
                } catch (e: Exception) {
                    Log.e("AWA", "Failed to set screen brightness", e)
                }
            }
            kotlinx.coroutines.delay(2000)
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        // Fullscreen Camera Preview
        Box(
            modifier = Modifier
                .fillMaxSize()
                .pointerInput(Unit) {
                    detectTapGestures {
                        camView.notifyScreenTapped()
                    }
                }
        ) {
            Preview(
                viewModel = camView,
                modifier = Modifier.fillMaxSize()
            )
        }

        // Top Controls Bar
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .align(Alignment.TopCenter)
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(16.dp)
            ) {
                // Exposure control
                Box {
                    TextButton(
                        onClick = {
                            showExposureSlider = !showExposureSlider
                            showFocusSlider = false
                        }
                    ) {
                        Text("EXP")
                    }

                    if (showExposureSlider) {
                        Popup(
                            alignment = Alignment.TopStart,
                            offset = IntOffset(0, 120),
                            onDismissRequest = { showExposureSlider = false }
                        ) {
                            Box(
                                modifier = Modifier
                                    .background(Color(0xCC1A1A1A), shape = RoundedCornerShape(8.dp))
                                    .padding(12.dp)
                            ) {
                                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                    Text(
                                        "Exposure: ${if (exposureIndex > 0) "+$exposureIndex" else "$exposureIndex"}",
                                        color = Color.White,
                                        fontSize = 12.sp
                                    )
                                    Slider(
                                        value = exposureIndex.toFloat(),
                                        onValueChange = { camView.setExposure(it.toInt()) },
                                        valueRange = exposureRange.first.toFloat()..exposureRange.last.toFloat(),
                                        modifier = Modifier.width(300.dp)
                                    )
                                }
                            }
                        }
                    }
                }

                // Focus control
                Box {
                    SmallFloatingActionButton(
                        onClick = {
                            camView.toggleFocusMode()
                            showFocusSlider = (settings.focusMode == CameraViewModel.FocusMode.MANUAL)
                        },
                        shape = CircleShape,
                        modifier = Modifier.size(36.dp)
                    ) {
                        Text(
                            text = if (focusMode == CameraViewModel.FocusMode.AUTO) "A" else "M",
                            fontSize = 16.sp
                        )
                    }

                    if (focusMode == CameraViewModel.FocusMode.MANUAL && showFocusSlider) {
                        Popup(
                            alignment = Alignment.TopStart,
                            offset = IntOffset(0, 120),
                            onDismissRequest = { showFocusSlider = false }
                        ) {
                            Box(
                                modifier = Modifier
                                    .background(Color(0xCC1A1A1A), shape = RoundedCornerShape(8.dp))
                                    .padding(12.dp)
                            ) {
                                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                    Text("Focus Distance", color = Color.White, fontSize = 12.sp)
                                    Slider(
                                        value = focusDistance,
                                        onValueChange = { camView.setFocusDistance(it) },
                                        valueRange = 0f..1f,
                                        modifier = Modifier.width(300.dp)
                                    )
                                }
                            }
                        }
                    }
                }

                // Flash toggle
                if (settings.hasFlashUnit) {
                    Box {
                        SmallFloatingActionButton(
                            onClick = {
                                camView.toggleFlash()
                            },
                            shape = CircleShape,
                            modifier = Modifier.size(36.dp)
                        ) {
                            if (settings.isFlashEnabled) {
                                Icon(Icons.Default.FlashOff, contentDescription = "Flash", Modifier.size(24.dp), tint = Color.White)
                            } else {
                                Icon(Icons.Default.FlashOn, contentDescription = "Flash", Modifier.size(24.dp), tint = Color.White)
                            }
                        }
                    }
                }
            }

            // Connection & Server IP status
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (isServerRunning) {
                    Box(modifier = Modifier, contentAlignment = Alignment.Center) {
                        Text("IP : ${ipAddress}:8554", modifier = Modifier.padding(4.dp))
                    }
                }
                Icon(
                    Icons.Default.Link,
                    contentDescription = "Server Status",
                    tint = if (isServerRunning) Color.Green else Color.Red,
                    modifier = Modifier.padding(horizontal = 2.dp)
                )
            }
        }

        // Bottom Controls Bar
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .align(Alignment.BottomCenter)
                .padding(horizontal = 16.dp)
                .padding(bottom = 32.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            // Left slot - Settings
            Box(modifier = Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
                IconButton(onClick = {
                    isSettingsOpen = true
                }) {
                    Icon(Icons.Default.Settings, contentDescription = "Settings", tint = Color.White)
                }
            }

            // Center slot - Shutter / Power
            FilledTonalButton(
                onClick = { camView.toggleServer() },
                contentPadding = PaddingValues(12.dp)
            ) {
                Icon(
                    imageVector = Icons.Default.PowerSettingsNew,
                    contentDescription = if (isServerRunning) "Stop server" else "Start server",
                    tint = if (isServerRunning) Color.Green else Color.Red,
                    modifier = Modifier.size(32.dp)
                )
            }

            // Right slot - Camera Switch
            Box(modifier = Modifier.weight(1f), contentAlignment = Alignment.CenterEnd) {
                IconButton(onClick = { camView.switchCamera() }) {
                    Icon(Icons.Default.Cameraswitch, contentDescription = "Flip Camera", tint = Color.White)
                }
            }
        }

        // Settings Drawer Panel
        SettingsPanel(
            isOpen = isSettingsOpen,
            onClose = { isSettingsOpen = false },
            camView = camView,
            modifier = Modifier.fillMaxSize(),
            settings = settings
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsPanel(
    isOpen: Boolean,
    onClose: () -> Unit,
    camView: CameraViewModel,
    modifier: Modifier = Modifier,
    settings: CameraViewModel.CameraSettings
) {
    val configuration = LocalConfiguration.current
    val isLandscape = configuration.orientation == Configuration.ORIENTATION_LANDSCAPE

    AnimatedVisibility(
        visible = isOpen,
        enter = slideInHorizontally(initialOffsetX = { it }),
        exit = slideOutHorizontally(targetOffsetX = { it }),
        modifier = modifier
    ) {
        Box {
            Box(
                modifier = Modifier
                    .fillMaxHeight()
                    .then(if (isLandscape) Modifier.fillMaxWidth(0.5f) else Modifier.fillMaxWidth())
                    .align(Alignment.CenterEnd)
                    .background(Color(0xCC1A1A1A))
                    .padding(24.dp)
            ) {
                Column {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Text("Settings", color = Color.White, style = MaterialTheme.typography.titleLarge)
                        IconButton(onClick = onClose) {
                            Icon(Icons.Default.Close, contentDescription = "Close", tint = Color.White)
                        }
                    }

                    Spacer(modifier = Modifier.height(24.dp))

                    VideoCodecSelector(camView = camView, settings = settings)

                    Spacer(modifier = Modifier.height(16.dp))

                    ResolutionDropdown(camView = camView, settings = settings)

                    Spacer(modifier = Modifier.height(16.dp))

                    FpsDropdown(camView = camView, settings = settings)

                    Spacer(modifier = Modifier.height(16.dp))

                    RotationDropdown(camView = camView, settings = settings)
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun VideoCodecSelector(camView: CameraViewModel, settings: CameraViewModel.CameraSettings) {
    var expanded by remember { mutableStateOf(false) }
    val currentCodec = settings.videoCodec.uppercase()

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = it }
    ) {
        OutlinedTextField(
            value = if (currentCodec == "H265" || currentCodec == "HEVC") "H.265 (HEVC)" else "H.264 (AVC)",
            onValueChange = {},
            readOnly = true,
            label = { Text("Hardware Video Codec") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
        )

        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false }
        ) {
            DropdownMenuItem(
                text = { Text("H.264 (AVC - High Performance)") },
                onClick = {
                    camView.setVideoCodec("h264")
                    expanded = false
                }
            )
            DropdownMenuItem(
                text = { Text("H.265 (HEVC - High Compression)") },
                onClick = {
                    camView.setVideoCodec("h265")
                    expanded = false
                }
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FpsDropdown(camView: CameraViewModel, settings: CameraViewModel.CameraSettings) {
    val currentFps = settings.fps
    var expanded by remember { mutableStateOf(false) }
    val options = listOf(
        15 to "15 fps (battery saver)",
        30 to "30 fps (smooth)"
    )
    val displayLabel = options.find { it.first == currentFps }?.second ?: "$currentFps fps"

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = it }
    ) {
        OutlinedTextField(
            value = displayLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text("Frame rate") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
        )

        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false }
        ) {
            options.forEach { (fps, label) ->
                DropdownMenuItem(
                    text = { Text(label) },
                    onClick = {
                        camView.setFps(fps)
                        expanded = false
                    }
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ResolutionDropdown(camView: CameraViewModel, settings: CameraViewModel.CameraSettings) {
    val currentResolution = settings.resolution
    val supportedResolutions by camView.supportedResolutions
    var expanded by remember { mutableStateOf(false) }

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = it }
    ) {
        OutlinedTextField(
            value = currentResolution.label,
            onValueChange = {},
            readOnly = true,
            label = { Text("Resolution") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
        )

        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false }
        ) {
            supportedResolutions.forEach { res ->
                DropdownMenuItem(
                    text = { Text(res.label) },
                    onClick = {
                        camView.setResolution(res)
                        expanded = false
                    }
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RotationDropdown(camView: CameraViewModel, settings: CameraViewModel.CameraSettings) {
    val currentMode = settings.rotationMode
    val rotationLabels = listOf(
        "auto" to "Auto (Sensor)",
        "0" to "0° (Normal)",
        "90" to "90°",
        "180" to "180° (Inverted)",
        "270" to "270°"
    )
    val displayLabel = rotationLabels.find { it.first == currentMode }?.second ?: currentMode
    var expanded by remember { mutableStateOf(false) }

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = it }
    ) {
        OutlinedTextField(
            value = displayLabel,
            onValueChange = {},
            readOnly = true,
            label = { Text("Rotation") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
        )

        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false }
        ) {
            rotationLabels.forEach { (mode, label) ->
                DropdownMenuItem(
                    text = { Text(label) },
                    onClick = {
                        camView.setRotation(mode)
                        expanded = false
                    }
                )
            }
        }
    }
}

fun getLocalIpAddress(): String? {
    try {
        val interfaces = NetworkInterface.getNetworkInterfaces()
        for (networkInterface in interfaces) {
            if (!networkInterface.isUp || networkInterface.isLoopback) continue
            for (address in networkInterface.inetAddresses) {
                if (address is Inet4Address && !address.isLoopbackAddress) {
                    return address.hostAddress
                }
            }
        }
    } catch (e: Exception) {
        Log.e("AWA", "Failed to get IP", e)
    }
    return null
}

@Composable
fun checkPermissions(
    vararg permissions: String = arrayOf(
        Manifest.permission.CAMERA
    )
): State<Boolean> {
    val context = LocalContext.current
    val isPreview = LocalInspectionMode.current

    if (isPreview) {
        return remember { mutableStateOf(true) }
    }

    fun isAllGranted() = permissions.all { permission ->
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED
    }

    val hasPermissions = remember(permissions) {
        mutableStateOf(isAllGranted())
    }

    val launcher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestMultiplePermissions()
    ) { results ->
        hasPermissions.value = results.values.all { it }
    }

    LaunchedEffect(permissions) {
        if (!hasPermissions.value) {
            launcher.launch(permissions.toList().toTypedArray())
        }
    }

    return hasPermissions
}
