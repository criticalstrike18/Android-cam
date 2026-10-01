package com.sjbtechnologies.awa.viewModel

import android.content.Context
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.util.Size
import android.view.MotionEvent
import android.view.OrientationEventListener
import android.view.View
import androidx.camera.core.CameraSelector
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableStateOf
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.pedro.common.ConnectChecker
import com.pedro.common.VideoCodec
import com.pedro.encoder.input.video.CameraHelper
import com.pedro.library.view.OpenGlView
import com.pedro.library.view.RenderErrorCallback
import com.pedro.rtspserver.RtspServerCamera2
import com.sjbtechnologies.awa.server.VideoStreamServer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.util.concurrent.atomic.AtomicInteger

class CameraViewModel : ViewModel() {

    enum class FocusMode {
        AUTO,
        MANUAL
    }

    enum class StreamResolution(
        val size: Size,
        val label: String
    ) {
        P480(Size(640, 480), "640x480 (480p)"),
        P720(Size(1280, 720), "1280x720 (720p)"),
        P1080(Size(1920, 1080), "1920x1080 (1080p)"),
        P1440(Size(2560, 1440), "2560x1440 (2K)"),
        P2160(Size(3840, 2160), "3840x2160 (4K)");

        companion object {
            fun fromString(str: String): StreamResolution? {
                val clean = str.trim().lowercase()
                return entries.firstOrNull {
                    val resKey = "${it.size.width}x${it.size.height}"
                    clean == resKey || clean.contains(resKey) || clean == it.name.lowercase()
                }
            }
        }
    }

    data class CameraSettings(
        val lensFacing: Int = CameraSelector.LENS_FACING_BACK,
        val resolution: StreamResolution = StreamResolution.P720,
        val fps: Int = 30,
        val zoom: Float = 1.0f,
        val focusMode: FocusMode = FocusMode.AUTO,
        val focusDistance: Float = 0f,
        val isFlashEnabled: Boolean = false,
        val exposureIndex: Int = 0,
        val exposureRange: IntRange = 0..0,
        val hasFlashUnit: Boolean = false,
        val rotationMode: String = "auto",
        val videoCodec: String = "h264"
    )

    private var appContext: Context? = null
    private var openGlView: OpenGlView? = null

    private val _supportedResolutions = mutableStateOf<List<StreamResolution>>(emptyList())
    val supportedResolutions: State<List<StreamResolution>> = _supportedResolutions

    private var orientationEventListener: OrientationEventListener? = null
    private var currentDeviceOrientation = 0

    private val _isPreviewActive = mutableStateOf(false)
    val isPreviewActive: State<Boolean> = _isPreviewActive

    private val _isServerRunning = mutableStateOf(false)
    val isServerRunning: State<Boolean> = _isServerRunning

    private val _showLocalPreview = mutableStateOf(true)
    val showLocalPreview: State<Boolean> = _showLocalPreview

    private val _settings = mutableStateOf(CameraSettings())
    val settings: State<CameraSettings> = _settings

    private val rtspPort = 8554
    private val streamLifecycleMutex = Mutex()

    private val viewerCount = AtomicInteger(0)
    private var rtspCamera: RtspServerCamera2? = null

    // Approach A robustness: CamX cold bring-up on SD730G-class SoCs takes ~10-13s, far beyond
    // RtspServer's hardcoded 5s SPS/PPS wait ("Video info is null"). A retry therefore performs a
    // FULL teardown and rebuild: startRtspCamera() constructs a brand-new RtspServerCamera2 every
    // attempt, so a same-instance restart is not possible (stopStream() tears down the encoder and
    // a subsequent startStream() then fails with "VideoEncoder not prepared yet").
    // Note: Camera2Base.isStreaming is set true before the SPS wait, so it stays true even after
    // a timeout — the only honest failure signal is onConnectionFailed("Video info is null").
    @Volatile private var streamRequested = false
    private var spsRetryJob: Job? = null
    private var spsRetryCount = 0
    private val maxSpsRetries = 5
    private val spsRetryDelayMs = 12_000L
    // Last-writer-wins start gate: concurrent starts (settings change vs view attach vs
    // watchdog heal) cancel the in-flight one instead of stacking full teardown cycles.
    // Stops always win — they cancel this job first.
    private var streamJob: Job? = null

    // Scoped self-healing (no other processes touched): if the framework closes OUR camera
    // device from underneath us (HAL wedge, cameraserver restart, resource preemption), the
    // availability callback fires for our ID while we still hold a live instance. We then
    // tear down and reopen ONLY our session. Our own orderly stops null rtspCamera first,
    // so they never trigger this path.
    private var availabilityCallback: CameraManager.AvailabilityCallback? = null

    fun initialize(context: Context) {
        appContext = context
        currentDeviceOrientation = CameraHelper.getCameraOrientation(context)

        orientationEventListener = object : OrientationEventListener(context) {
            override fun onOrientationChanged(orientation: Int) {
                if (orientation == ORIENTATION_UNKNOWN) return
                val newOrientation = when (orientation) {
                    in 45..134 -> 270
                    in 135..224 -> 180
                    in 225..314 -> 90
                    else -> 0
                }
                if (newOrientation != currentDeviceOrientation) {
                    currentDeviceOrientation = newOrientation
                    // "auto" is the client default, so the physical orientation has to reach
                    // the encoder here. Previously this value was only reported in /settings and
                    // never applied, which is why a phone lying flat streamed sideways.
                    if (_settings.value.rotationMode == "auto") {
                        applyStreamRotation()
                    }
                }
            }
        }
        orientationEventListener?.enable()

        updateSupportedResolutions(context, _settings.value.lensFacing)

        VideoStreamServer.featuresProvider = {
            val s = _settings.value
            VideoStreamServer.FeaturesResponse(
                resolutions = _supportedResolutions.value.map { "${it.size.width}x${it.size.height}" },
                manual_focus = true,
                exposure_upper = s.exposureRange.endInclusive,
                exposure_lower = s.exposureRange.start,
                has_zoom = true,
                zoom_max = 5.0f,
                zoom_min = 1.0f,
                stream_protocol = "rtsp",
                server_port = 8080,
                rtsp_port = rtspPort,
                current_rotation = s.rotationMode,
                available_codecs = listOf("h264", "h265")
            )
        }

        VideoStreamServer.settingsProvider = {
            val s = _settings.value
            val camStr = if (s.lensFacing == CameraSelector.LENS_FACING_FRONT) "front" else "back"
            val effectiveRotation = if (s.rotationMode == "auto") {
                currentDeviceOrientation.toString()
            } else {
                s.rotationMode
            }
            VideoStreamServer.SettingsResponse(
                camera = camStr,
                resolution_str = "${s.resolution.size.width}x${s.resolution.size.height}",
                zoom = s.zoom,
                focus_mode = if (s.focusMode == FocusMode.AUTO) 0 else 1,
                focus_distance = s.focusDistance * 1000f,
                exposure_index = s.exposureIndex,
                autofocus = (s.focusMode == FocusMode.AUTO),
                stream_quality = 80,
                flash = s.isFlashEnabled,
                has_flash_unit = s.hasFlashUnit,
                stream_protocol = "rtsp",
                rotation = effectiveRotation,
                supported_resolutions = _supportedResolutions.value.map { "${it.size.width}x${it.size.height}" },
                video_codec = s.videoCodec,
                fps = s.fps
            )
        }

        VideoStreamServer.onSettingsUpdated = { req ->
            // Suspends until the main thread has actually applied the change. This previously
            // launched a coroutine and returned immediately, so POST /settings always
            // acknowledged and broadcast the OLD state, and GET /control re-read the OLD state.
            withContext(Dispatchers.Main) {
                req.camera?.let { setCameraFacing(it.equals("front", ignoreCase = true)) }
                // `switch_camera` toggles, so an explicit `camera` in the same request would
                // immediately undo it (?camera=front&switch_camera=true cancelled itself out).
                req.switchCamera?.let { if (it && req.camera == null) switchCamera() }
                req.resolution_str?.let { StreamResolution.fromString(it)?.let { res -> setResolution(res) } }
                req.zoom?.let { setZoom(it) }
                req.exposure_index?.let { setExposure(it) }
                req.focus_distance?.let { setFocusDistance((it / 1000f).coerceIn(0f, 1f)) }
                req.focus_mode?.let {
                    if (it == 0) cancelFocusAndMetering()
                    else _settings.value = _settings.value.copy(focusMode = FocusMode.MANUAL)
                }
                req.autofocus?.let {
                    if (it) cancelFocusAndMetering()
                    else _settings.value = _settings.value.copy(focusMode = FocusMode.MANUAL)
                }
                req.flash?.let { applyFlash(it) }
                req.rotation?.let { setRotation(it) }
                req.video_codec?.let { setVideoCodec(it) }
                req.fps?.let { setFps(it) }
            }
            null
        }

        VideoStreamServer.onUserConnected = {
            Log.d("AWA", "Active viewer connected. Current viewers: ${viewerCount.get()}")
        }

        VideoStreamServer.onUserDisconnected = {
            Log.d("AWA", "Viewer disconnected. Remaining viewers: ${viewerCount.get()}")
        }

        VideoStreamServer.onServerStateChanged = { running ->
            _isServerRunning.value = running
        }

        updateCameraRanges(context, _settings.value.lensFacing)

        registerAvailabilityWatchdog(context)

        // Always ensure the REST/WebSocket control plane is up so desktop connects seamlessly.
        // Approach A: NEVER start RTSP in background mode (openGlView == null) — that path
        // leaves startPreview() as a no-op and races CamX bring-up (~5-6s on SD730G) against
        // RtspServer's 5s SPS/PPS wait ("Video info is null"). Defer RTSP until the visible
        // OpenGlView is attached so startPreview() pumps frames immediately.
        VideoStreamServer.start(8080)
        streamRequested = true
        if (openGlView != null) {
            startStream()
        } else {
            Log.d("AWA", "Deferring RTSP start until OpenGlView is attached (Approach A)")
        }
    }

    fun attachOpenGlView(view: OpenGlView) {
        val prev = openGlView
        if (prev === view) return
        // If we are swapping to a brand-new surface while a background-mode camera exists,
        // tear it down first so the next start binds to the visible surface.
        val needsRestart = prev == null && appContext != null
        openGlView = view
        if (needsRestart) {
            startStream()
        }
    }

    fun toggleServer() {
        if (_isServerRunning.value || streamRequested) {
            stopActiveStream()
        } else {
            if (!VideoStreamServer.isRunning) {
                VideoStreamServer.start(8080)
            }
            streamRequested = true
            spsRetryCount = 0
            startStream()
        }
    }

    fun detachOpenGlView() {
        openGlView = null
    }

    private fun resolveActiveCameraId(): String? {
        val context = appContext ?: return null
        return try {
            val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val targetFacing = if (_settings.value.lensFacing == CameraSelector.LENS_FACING_FRONT) {
                CameraCharacteristics.LENS_FACING_FRONT
            } else {
                CameraCharacteristics.LENS_FACING_BACK
            }
            manager.cameraIdList.firstOrNull { id ->
                manager.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.LENS_FACING) == targetFacing
            }
        } catch (e: Exception) {
            Log.e("AWA", "resolveActiveCameraId failed", e)
            null
        }
    }

    private fun registerAvailabilityWatchdog(context: Context) {
        if (availabilityCallback != null) return
        try {
            val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val callback = object : CameraManager.AvailabilityCallback() {
                override fun onCameraAvailable(cameraId: String) {
                    // Fires when OUR device is closed from underneath us while we still hold it
                    // (framework disconnect / HAL wedge / cameraserver restart). Our own orderly
                    // stops null rtspCamera before the close completes, so they can't reach here.
                    if (!streamRequested || rtspCamera == null) return
                    val ours = resolveActiveCameraId() ?: return
                    if (cameraId == ours) {
                        Log.w("AWA", "Framework closed our camera $cameraId — self-healing own session")
                        healCameraSession("framework closed camera $cameraId")
                    }
                }

                override fun onCameraUnavailable(cameraId: String) {
                    Log.d("AWA", "Camera $cameraId unavailable")
                }
            }
            manager.registerAvailabilityCallback(callback, Handler(Looper.getMainLooper()))
            availabilityCallback = callback
            Log.d("AWA", "Camera availability watchdog registered")
        } catch (e: Exception) {
            Log.e("AWA", "Failed to register availability watchdog", e)
        }
    }

    private fun healCameraSession(reason: String) {
        Log.d("AWA", "Healing camera session: $reason")
        spsRetryCount = 0
        _isServerRunning.value = false
        spsRetryJob?.cancel()
        spsRetryJob = null
        requestStreamStart()
    }

    /** Release our camera when backgrounded so we never squat on (or wedge) the shared HAL. */
    fun pauseCamera() {
        Log.d("AWA", "pauseCamera: app backgrounded, releasing our camera session")
        spsRetryJob?.cancel()
        spsRetryJob = null
        streamJob?.cancel()
        streamJob = null
        _isServerRunning.value = false
        _isPreviewActive.value = false
        viewModelScope.launch(Dispatchers.Main) {
            streamLifecycleMutex.withLock {
                stopRtspCamera()
            }
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    /** Reacquire our camera when foregrounded if streaming was requested. */
    fun resumeCamera() {
        Log.d("AWA", "resumeCamera: app foregrounded, streamRequested=$streamRequested")
        if (streamRequested && openGlView != null) {
            spsRetryCount = 0
            spsRetryJob?.cancel()
            spsRetryJob = null
            requestStreamStart()
        }
    }

    private fun updateCameraRanges(context: Context, lensFacing: Int) {
        try {
            val cameraManager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val targetFacing = if (lensFacing == CameraSelector.LENS_FACING_FRONT) {
                CameraCharacteristics.LENS_FACING_FRONT
            } else {
                CameraCharacteristics.LENS_FACING_BACK
            }

            var selectedCameraId: String? = null
            for (id in cameraManager.cameraIdList) {
                val characteristics = cameraManager.getCameraCharacteristics(id)
                if (characteristics.get(CameraCharacteristics.LENS_FACING) == targetFacing) {
                    selectedCameraId = id
                    break
                }
            }

            if (selectedCameraId != null) {
                val characteristics = cameraManager.getCameraCharacteristics(selectedCameraId)
                val range = characteristics.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
                val flash = characteristics.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) ?: false
                if (range != null) {
                    _settings.value = _settings.value.copy(
                        exposureRange = range.lower..range.upper,
                        hasFlashUnit = flash
                    )
                }
            }
        } catch (e: Exception) {
            Log.e("AWA", "Failed to query camera ranges", e)
        }
    }

    fun setVideoCodec(codec: String) {
        val clean = codec.lowercase().trim()
        if (_settings.value.videoCodec == clean) return
        _settings.value = _settings.value.copy(videoCodec = clean)
        restartRtspCamera("Video codec changed to $clean")
    }

    private fun updateSupportedResolutions(context: Context, lensFacing: Int) {
        try {
            val cameraManager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
            val targetFacing = if (lensFacing == CameraSelector.LENS_FACING_FRONT) {
                CameraCharacteristics.LENS_FACING_FRONT
            } else {
                CameraCharacteristics.LENS_FACING_BACK
            }

            var selectedCameraId: String? = null
            for (id in cameraManager.cameraIdList) {
                val characteristics = cameraManager.getCameraCharacteristics(id)
                if (characteristics.get(CameraCharacteristics.LENS_FACING) == targetFacing) {
                    selectedCameraId = id
                    break
                }
            }

            if (selectedCameraId != null) {
                val characteristics = cameraManager.getCameraCharacteristics(selectedCameraId)
                val map = characteristics.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
                val outputSizes = map?.getOutputSizes(android.graphics.ImageFormat.JPEG) ?: emptyArray()

                val supported = StreamResolution.entries.filter { res ->
                    if (res == StreamResolution.P2160 && targetFacing == CameraCharacteristics.LENS_FACING_BACK) {
                        true // Hardware 4K UHD video encoding natively supported on rear camera
                    } else {
                        outputSizes.any { size ->
                            size.width == res.size.width && size.height == res.size.height
                        }
                    }
                }
                _supportedResolutions.value = if (supported.isNotEmpty()) supported else listOf(StreamResolution.P720)
            } else {
                _supportedResolutions.value = listOf(StreamResolution.P720)
            }
        } catch (e: Exception) {
            Log.e("AWA", "Failed to query supported resolutions", e)
            _supportedResolutions.value = listOf(StreamResolution.P720)
        }
    }

    private fun buildConnectChecker() = object : ConnectChecker {
        override fun onConnectionStarted(url: String) {
            Log.d("AWA", "RTSP connection started: $url")
        }

        override fun onConnectionSuccess() {
            Log.d("AWA", "RTSP client connected successfully")
            _isPreviewActive.value = true
            // A completed client handshake proves SPS/PPS are live — reset the retry budget.
            spsRetryCount = 0
            spsRetryJob?.cancel()
            spsRetryJob = null
            viewerCount.incrementAndGet()
            VideoStreamServer.onUserConnected?.invoke()
        }

        override fun onConnectionFailed(reason: String) {
            Log.e("AWA", "RTSP connection failed: $reason")
            _isPreviewActive.value = false
            if (reason.contains("Video info is null", ignoreCase = true)) {
                // Honest status: the server socket was never created (bytecode-verified in
                // RtspServer$startServer$1: 5000ms semaphore wait, then abort).
                _isServerRunning.value = false
                scheduleSpsRetry()
            }
        }

        override fun onNewBitrate(bitrate: Long) {
            Log.d("AWA", "RTSP new bitrate: $bitrate")
        }

        override fun onDisconnect() {
            Log.d("AWA", "RTSP client disconnected")
            _isPreviewActive.value = false
            viewerCount.decrementAndGet().coerceAtLeast(0)
            VideoStreamServer.onUserDisconnected?.invoke()
        }

        override fun onAuthError() {
            Log.e("AWA", "RTSP Auth error")
        }

        override fun onAuthSuccess() {
            Log.d("AWA", "RTSP Auth success")
        }
    }

    private suspend fun startRtspCamera() {
        appContext ?: return
        val view = openGlView
        if (view == null) {
            Log.w("AWA", "Skipping RTSP start: no OpenGlView attached yet (background mode disabled)")
            _isServerRunning.value = false
            return
        }
        // The view can be mid-swap (Compose recomposition racing a settings-triggered start):
        // starting GL on a detached view throws IllegalArgumentException on the main thread
        // and kills the app. Defer gracefully instead — the retry loop picks it up.
        try {
            if (view.holder?.surface?.isValid != true) {
                Log.w("AWA", "Skipping RTSP start: OpenGlView surface not yet valid")
                _isServerRunning.value = false
                scheduleSpsRetry()
                return
            }
        } catch (e: Exception) {
            Log.w("AWA", "Skipping RTSP start: cannot inspect view surface", e)
            _isServerRunning.value = false
            scheduleSpsRetry()
            return
        }
        Log.d("AWA", "Initializing RtspServerCamera2 with OpenGlView")
        val camera = RtspServerCamera2(view, buildConnectChecker(), rtspPort)
        rtspCamera = camera

        val renderErrorCallback = object : RenderErrorCallback {
            override fun onRenderError(e: RuntimeException) {
                Log.e("AWA", "FATAL GL RENDER ERROR:", e)
            }
        }
        (camera.glInterface as? OpenGlView)?.setRenderErrorCallback(renderErrorCallback)

        val s = _settings.value
        val isHevc = s.videoCodec.equals("h265", ignoreCase = true) || s.videoCodec.equals("hevc", ignoreCase = true)

        camera.setVideoCodec(if (isHevc) VideoCodec.H265 else VideoCodec.H264)

        var actualRes = s.resolution
        // Keep encoder orientation at 0 (landscape) to match Camera2 sensor output and avoid HAL fence mismatch
        val orientation = 0

        var videoOk = false
        val bitrate = when (actualRes) {
            StreamResolution.P2160 -> if (isHevc) 12_000 * 1024 else 22_000 * 1024
            StreamResolution.P1440 -> if (isHevc) 5_500 * 1024 else 10_000 * 1024
            StreamResolution.P1080 -> if (isHevc) 3_500 * 1024 else 6_000 * 1024
            StreamResolution.P720  -> if (isHevc) 1_800 * 1024 else 3_000 * 1024
            StreamResolution.P480  -> 1_000 * 1024
        }

        val fps = s.fps.coerceIn(1, 60)
        try {
            videoOk = camera.prepareVideo(
                actualRes.size.width,
                actualRes.size.height,
                fps,
                bitrate,
                2,
                orientation
            )
        } catch (e: Exception) {
            Log.e("AWA", "RTSP prepareVideo failed for ${actualRes.label}", e)
        }

        if (!videoOk) {
            val fallbackChain = listOf(
                StreamResolution.P1080,
                StreamResolution.P720,
                StreamResolution.P480
            )
            for (fallback in fallbackChain) {
                if (fallback.size.width < actualRes.size.width) {
                    val fallbackBitrate = when (fallback) {
                        StreamResolution.P1080 -> if (isHevc) 3_500 * 1024 else 6_000 * 1024
                        StreamResolution.P720  -> if (isHevc) 1_800 * 1024 else 3_000 * 1024
                        else                   -> 1_000 * 1024
                    }
                    val fallbackOk = try {
                        camera.prepareVideo(fallback.size.width, fallback.size.height, fps, fallbackBitrate, 2, orientation)
                    } catch (_: Exception) { false }
                    if (fallbackOk) {
                        videoOk = true
                        actualRes = fallback
                        _settings.value = _settings.value.copy(resolution = fallback)
                        break
                    }
                }
            }
        }

        Log.d("AWA", "RTSP prepareVideo=$videoOk at ${actualRes.label} (audio disabled)")

        if (videoOk) {
            val facing = if (s.lensFacing == CameraSelector.LENS_FACING_FRONT) {
                CameraHelper.Facing.FRONT
            } else {
                CameraHelper.Facing.BACK
            }

            if (!camera.isOnPreview) {
                try {
                    camera.startPreview(facing, actualRes.size.width, actualRes.size.height, fps, 0)
                } catch (e: Exception) {
                    // startPreview throws on the calling (main) thread when the GL surface is
                    // gone (view swap, activity teardown). Never let this kill the app.
                    Log.e("AWA", "startPreview failed (view surface likely swapped)", e)
                    _isServerRunning.value = false
                    rtspCamera = null
                    scheduleSpsRetry()
                    return
                }
                // Let Camera2/CamX deliver the first frames so the encoder emits SPS/PPS
                // BEFORE RtspServer blocks on its 5s "waiting for video info" wait.
                // With the preview already pumping at 30fps, startStream() then latches in ms.
                delay(2500)
            }

            try {
                camera.startStream()
            } catch (e: Exception) {
                // Synchronous start failure (e.g. CameraOpenException after a cameraserver-side
                // disconnect): no server was created, so no async SPS verdict will arrive.
                Log.e("AWA", "Error starting RTSP stream", e)
                _isServerRunning.value = false
                scheduleSpsRetry()
                return
            }

            applyStreamRotation()

            // Optimistic: isStreaming is set before the async 5s SPS wait, so it cannot
            // confirm success. onConnectionFailed("Video info is null") corrects this to false
            // and triggers a warm retry via scheduleSpsRetry().
            _isServerRunning.value = true
        } else {
            Log.e("AWA", "Could not prepare RTSP video stream.")
            _isServerRunning.value = false
        }
    }

    private fun stopRtspCamera() {
        try {
            rtspCamera?.let { camera ->
                if (camera.isStreaming) {
                    camera.stopStream()
                }
                if (camera.isOnPreview) {
                    camera.stopPreview()
                }
            }
        } catch (e: Exception) {
            Log.e("AWA", "Error stopping RTSP camera", e)
        } finally {
            rtspCamera = null
        }
        _isServerRunning.value = false
        _isPreviewActive.value = false
    }

    /**
     * Rotation actually applied to the encoder output.
     *
     * "auto" tracks the physical device orientation reported by [orientationEventListener];
     * any other value is an explicit degrees setting. An unparseable value falls back to 0.
     */
    private fun effectiveStreamRotation(): Int {
        val mode = _settings.value.rotationMode
        return if (mode == "auto") {
            currentDeviceOrientation
        } else {
            mode.toIntOrNull() ?: 0
        }
    }

    /**
     * Pushes the effective rotation to the GL pipeline.
     *
     * Note: this deliberately targets [com.pedro.library.view.GlInterface] via [OpenGlView].
     * The camera is constructed with the [OpenGlView] constructor, so `glInterface` is that
     * view and NOT a GlStreamInterface - the interface implemented by the background
     * constructor. Casting to `GlStreamInterface` here always yielded null, which left
     * `rotation=auto` (the client default) with no effect on the encoded stream.
     */
    private fun applyStreamRotation() {
        val rotation = effectiveStreamRotation()
        val gl = rtspCamera?.glInterface ?: return
        try {
            (gl as? OpenGlView)?.setStreamRotation(rotation)
        } catch (e: Exception) {
            Log.e("AWA", "Failed to apply stream rotation $rotation", e)
        }
    }

    fun setPreviewResolution(width: Int, height: Int) {
        // Handled automatically by GL preview
    }

    fun startStream() {
        spsRetryJob?.cancel()
        spsRetryJob = null
        requestStreamStart()
    }

    /**
     * Single gate for every (re)start: full restart (stop → re-prepare → preview → stream).
     * Last-writer-wins: a newer request cancels the in-flight one so concurrent triggers
     * (settings change vs view attach vs watchdog heal) can't stack teardown cycles and race
     * on the shared OpenGlView surface. [settleMs] lets SPS retries give CamX teardown room
     * (ERROR_CAMERA_DEVICE disconnects were seen when re-opening too fast).
     */
    private fun requestStreamStart(settleMs: Long = 300) {
        streamJob?.cancel()
        streamJob = viewModelScope.launch(Dispatchers.Main) {
            streamLifecycleMutex.withLock {
                stopRtspCamera()
                delay(settleMs)
                startRtspCamera()
            }
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    private fun stopActiveStream() {
        streamRequested = false
        spsRetryJob?.cancel()
        spsRetryJob = null
        spsRetryCount = 0
        streamJob?.cancel()
        streamJob = null
        viewModelScope.launch(Dispatchers.Main) {
            streamLifecycleMutex.withLock {
                stopRtspCamera()
            }
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    private fun restartRtspCamera(reason: String) {
        Log.d("AWA", "Restarting RTSP camera: $reason")
        spsRetryCount = 0
        startStream()
    }

    private fun scheduleSpsRetry() {
        if (!streamRequested) {
            Log.d("AWA", "SPS retry skipped: stream not requested")
            return
        }
        if (spsRetryCount >= maxSpsRetries) {
            Log.e("AWA", "SPS retry budget exhausted ($spsRetryCount attempts). Tap power to retry manually.")
            return
        }
        if (spsRetryJob?.isActive == true) return
        spsRetryCount++
        Log.d("AWA", "Scheduling SPS retry #$spsRetryCount in ${spsRetryDelayMs}ms")
        spsRetryJob = viewModelScope.launch(Dispatchers.Main) {
            delay(spsRetryDelayMs)
            if (!streamRequested || openGlView == null) {
                Log.d("AWA", "SPS retry aborted: streamRequested=$streamRequested")
                return@launch
            }
            // Full restart via the single gate (cancels anything in flight).
            requestStreamStart(settleMs = 1500)
        }
    }

    private val _lastInteractionMs = mutableStateOf(android.os.SystemClock.uptimeMillis())
    val lastInteractionMs: State<Long> = _lastInteractionMs

    fun notifyScreenTapped() {
        _showLocalPreview.value = true
        _lastInteractionMs.value = android.os.SystemClock.uptimeMillis()
    }

    fun tapToFocus(view: View, event: MotionEvent) {
        try {
            rtspCamera?.tapToFocus(view, event)
        } catch (e: Exception) {
            Log.e("AWA", "tapToFocus RTSP failed", e)
        }
    }

    fun toggleFocusMode() {
        if (_settings.value.focusMode == FocusMode.AUTO) {
            _settings.value = _settings.value.copy(focusMode = FocusMode.MANUAL)
        } else {
            cancelFocusAndMetering()
        }
    }

    fun setCameraFacing(front: Boolean) {
        val targetFacing = if (front) CameraSelector.LENS_FACING_FRONT else CameraSelector.LENS_FACING_BACK
        if (_settings.value.lensFacing == targetFacing) return
        _settings.value = _settings.value.copy(lensFacing = targetFacing)
        appContext?.let { updateSupportedResolutions(it, targetFacing) }

        viewModelScope.launch(Dispatchers.Main) {
            rtspCamera?.let { camera ->
                try {
                    val targetCameraHelperFacing = if (front) CameraHelper.Facing.FRONT else CameraHelper.Facing.BACK
                    if (camera.cameraFacing != targetCameraHelperFacing) {
                        camera.switchCamera()
                    }
                } catch (e: Exception) {
                    Log.e("AWA", "camera.switchCamera failed, restarting RTSP", e)
                    restartRtspCamera("Camera facing changed")
                }
            }
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    fun switchCamera() {
        val isFront = _settings.value.lensFacing == CameraSelector.LENS_FACING_FRONT
        setCameraFacing(!isFront)
    }

    fun setFocusDistance(distance: Float) {
        val clamped = distance.coerceIn(0f, 1f)
        _settings.value = _settings.value.copy(
            focusDistance = clamped,
            focusMode = FocusMode.MANUAL
        )
        rtspCamera?.setFocusDistance(clamped)
    }

    fun cancelFocusAndMetering() {
        _settings.value = _settings.value.copy(focusMode = FocusMode.AUTO)
        rtspCamera?.let { camera ->
            try {
                camera.setFocusDistance(0f)
            } catch (e: Exception) {
                Log.e("AWA", "cancelFocusAndMetering failed", e)
            }
        }
    }

    fun setRotation(mode: String) {
        _settings.value = _settings.value.copy(rotationMode = mode)
        applyStreamRotation()
        viewModelScope.launch {
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    fun applyFlash(enabled: Boolean) {
        _settings.value = _settings.value.copy(isFlashEnabled = enabled)
        rtspCamera?.let { camera ->
            try {
                if (enabled) camera.enableLantern() else camera.disableLantern()
            } catch (e: Exception) {
                Log.e("AWA", "Failed to set RTSP flash", e)
            }
        }
    }

    fun toggleFlash() {
        applyFlash(!_settings.value.isFlashEnabled)
    }

    fun setResolution(res: StreamResolution) {
        if (_settings.value.resolution == res) return
        _settings.value = _settings.value.copy(resolution = res)
        restartRtspCamera("Resolution changed to ${res.label}")
    }

    fun setFps(fps: Int) {
        // 15fps halves sensor/ISP/encoder/radio work vs 30fps — the biggest
        // battery lever after the screen. Constrain to sane streaming rates.
        val v = if (fps <= 20) 15 else 30
        if (_settings.value.fps == v) return
        _settings.value = _settings.value.copy(fps = v)
        restartRtspCamera("Frame rate changed to ${v}fps")
    }

    /** Zoom clamped to the advertised range. Reachable unauthenticated over HTTP, so both the
     *  range clamp and a driver-level failure are handled rather than thrown on the main thread. */
    fun setZoom(ratio: Float) {
        val clamped = ratio.coerceIn(1.0f, 5.0f)
        viewModelScope.launch(Dispatchers.Main) {
            _settings.value = _settings.value.copy(zoom = clamped)
            try {
                rtspCamera?.setZoom(clamped)
            } catch (e: Exception) {
                // A device whose SCALER_AVAILABLE_MAX_DIGITAL_ZOOM is below the advertised 5.0
                // throws here. Keep the state honest rather than crashing.
                Log.e("AWA", "setZoom($clamped) rejected", e)
                _settings.value = _settings.value.copy(zoom = 1.0f)
            }
        }
    }

    /**
     * Exposure compensation index.
     *
     * The value is clamped to the range reported by CONTROL_AE_COMPENSATION_RANGE *before* it
     * reaches the camera. Passing an out-of-range index straight through produced a driver-level
     * IllegalArgumentException on the main thread, reachable from any LAN client via
     * `POST /settings {"exposure_index":9999}`.
     */
    fun setExposure(index: Int) {
        val range = _settings.value.exposureRange
        val clamped = if (range.first > range.last) index else index.coerceIn(range.first, range.last)
        _settings.value = _settings.value.copy(exposureIndex = clamped)
        try {
            rtspCamera?.setExposure(clamped)
        } catch (e: Exception) {
            Log.e("AWA", "setExposure($clamped) rejected", e)
        }
    }

    override fun onCleared() {
        super.onCleared()
        streamRequested = false
        spsRetryJob?.cancel()
        spsRetryJob = null
        streamJob?.cancel()
        streamJob = null
        orientationEventListener?.disable()
        try {
            val context = appContext
            val callback = availabilityCallback
            if (context != null && callback != null) {
                (context.getSystemService(Context.CAMERA_SERVICE) as CameraManager)
                    .unregisterAvailabilityCallback(callback)
            }
        } catch (e: Exception) {
            Log.e("AWA", "Failed to unregister availability watchdog", e)
        } finally {
            availabilityCallback = null
        }
        stopRtspCamera()
        VideoStreamServer.stop()
    }
}
