package com.sjbtechnologies.awa.viewModel

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Matrix
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.util.Size
import android.view.MotionEvent
import android.view.OrientationEventListener
import android.view.Surface
import android.view.View
import androidx.camera.core.*
import androidx.camera.core.resolutionselector.AspectRatioStrategy
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableStateOf
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.pedro.common.ConnectChecker
import com.pedro.encoder.input.video.CameraHelper
import com.pedro.library.view.GlStreamInterface
import com.pedro.rtspserver.RtspServerCamera2
import com.sjbtechnologies.awa.server.VideoStreamServer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.ByteArrayOutputStream
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

class CameraViewModel : ViewModel() {

    enum class StreamMode(val label: String) {
        H264_RTSP("H264 RTSP"),
        MJPEG("MJPEG")
    }

    enum class FocusMode {
        AUTO,
        MANUAL
    }

    enum class StreamResolution(
        val size: Size,
        val label: String,
        val aspectRatio: Int = AspectRatio.RATIO_16_9
    ) {
        P480(Size(640, 480), "640x480 (480p)", AspectRatio.RATIO_4_3),
        P720(Size(1280, 720), "1280x720 (720p)", AspectRatio.RATIO_16_9),
        P1080(Size(1920, 1080), "1920x1080 (1080p)", AspectRatio.RATIO_16_9),
        P1440(Size(2560, 1440), "2560x1440 (2K)", AspectRatio.RATIO_16_9),
        P2160(Size(3840, 2160), "3840x2160 (4K)", AspectRatio.RATIO_16_9);

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
        val zoom: Float = 1.0f,
        val focusMode: FocusMode = FocusMode.AUTO,
        val focusDistance: Float = 0f,
        val isFlashEnabled: Boolean = false,
        val exposureIndex: Int = 0,
        val exposureRange: IntRange = 0..0,
        val hasFlashUnit: Boolean = false,
        val jpegQuality: Int = 80,
        val rotationMode: String = "auto"
    )

    private var appContext: Context? = null
    private var lifecycleOwner: LifecycleOwner? = null
    private var previewView: PreviewView? = null

    private val _supportedResolutions = mutableStateOf<List<StreamResolution>>(emptyList())
    val supportedResolutions: State<List<StreamResolution>> = _supportedResolutions

    private var orientationEventListener: OrientationEventListener? = null
    private var currentDeviceOrientation = 0
    private var imageAnalysisUseCase: ImageAnalysis? = null

    private val _isPreviewActive = mutableStateOf(false)
    val isPreviewActive: State<Boolean> = _isPreviewActive

    private val _isServerRunning = mutableStateOf(false)
    val isServerRunning: State<Boolean> = _isServerRunning

    private val _showLocalPreview = mutableStateOf(true)
    val showLocalPreview: State<Boolean> = _showLocalPreview

    private var hidePreviewRunnable: Runnable? = null
    private val previewPeekDurationMs = 5000L
    private val mainHandler = Handler(Looper.getMainLooper())

    private var cameraProvider: ProcessCameraProvider? = null
    private var cameraExecutor: ExecutorService? = null

    private val _settings = mutableStateOf(CameraSettings())
    val settings: State<CameraSettings> = _settings

    private val _streamMode = mutableStateOf(StreamMode.H264_RTSP)
    val streamMode: State<StreamMode> = _streamMode

    private val rtspPort = 8554
    private val rtspBitrate = 2500 * 1024
    private var activePreviewSurface: Surface? = null
    private val streamLifecycleMutex = Mutex()

    private val viewerCount = AtomicInteger(0)
    private val jpegViewerCount = AtomicInteger(0)

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
                    if (_settings.value.rotationMode == "auto" && _streamMode.value == StreamMode.MJPEG) {
                        bindCamera()
                        viewModelScope.launch {
                            VideoStreamServer.broadcastSettingsUpdate()
                        }
                    }
                }
            }
        }
        orientationEventListener?.enable()

        updateSupportedResolutions(context, _settings.value.lensFacing)

        VideoStreamServer.featuresProvider = {
            val s = _settings.value
            val proto = if (_streamMode.value == StreamMode.H264_RTSP) "rtsp" else "mjpeg"
            val portRtsp = if (_streamMode.value == StreamMode.H264_RTSP) rtspPort else null
            VideoStreamServer.FeaturesResponse(
                resolutions = _supportedResolutions.value.map { "${it.size.width}x${it.size.height}" },
                manual_focus = true,
                exposure_upper = s.exposureRange.endInclusive,
                exposure_lower = s.exposureRange.start,
                has_zoom = true,
                zoom_max = 5.0f,
                zoom_min = 1.0f,
                stream_protocol = proto,
                server_port = 8080,
                rtsp_port = portRtsp,
                current_rotation = s.rotationMode
            )
        }

        VideoStreamServer.settingsProvider = {
            val s = _settings.value
            val proto = if (_streamMode.value == StreamMode.H264_RTSP) "rtsp" else "mjpeg"
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
                stream_quality = s.jpegQuality,
                flash = s.isFlashEnabled,
                has_flash_unit = s.hasFlashUnit,
                stream_protocol = proto,
                rotation = effectiveRotation,
                supported_resolutions = _supportedResolutions.value.map { "${it.size.width}x${it.size.height}" }
            )
        }

        VideoStreamServer.onSettingsUpdated = { update ->
            viewModelScope.launch(Dispatchers.Main) {
                update.resolution_str?.let { resStr ->
                    StreamResolution.fromString(resStr)?.let { setResolution(it) }
                }
                update.camera?.let { cam ->
                    val front = cam.equals("front", ignoreCase = true)
                    setCameraFacing(front)
                }
                if (update.switchCamera == true) {
                    switchCamera()
                }
                update.stream_protocol?.let { proto ->
                    if (proto.equals("rtsp", ignoreCase = true)) {
                        setStreamMode(StreamMode.H264_RTSP)
                    } else if (proto.equals("mjpeg", ignoreCase = true)) {
                        setStreamMode(StreamMode.MJPEG)
                    }
                }
                update.rotation?.let { rot ->
                    setRotation(rot)
                }
                update.flash?.let { flash ->
                    applyFlash(flash)
                }
                update.zoom?.let { zoom ->
                    setZoom(zoom)
                }
                update.exposure_index?.let { exp ->
                    setExposure(exp)
                }
                update.focus_mode?.let { fMode ->
                    if (fMode == 0) {
                        cancelFocusAndMetering()
                        _settings.value = _settings.value.copy(focusMode = FocusMode.AUTO)
                    } else {
                        _settings.value = _settings.value.copy(focusMode = FocusMode.MANUAL)
                    }
                }
                update.focus_distance?.let { dist ->
                    setFocusDistance(dist / 1000f)
                }
                update.stream_quality?.let { q ->
                    setQuality(q)
                }
                VideoStreamServer.broadcastSettingsUpdate()
            }
            null
        }

        VideoStreamServer.onVideoViewerConnected = {
            val count = jpegViewerCount.incrementAndGet()
            viewerCount.incrementAndGet()
            if (_isServerRunning.value && _streamMode.value == StreamMode.MJPEG) {
                viewModelScope.launch(Dispatchers.Main) {
                    _isPreviewActive.value = true
                    if (count == 1) {
                        bindCamera()
                    }
                }
            }
        }

        VideoStreamServer.onVideoViewerDisconnected = {
            val remaining = jpegViewerCount.decrementAndGet().coerceAtLeast(0)
            viewerCount.updateAndGet { (it - 1).coerceAtLeast(0) }
            if (_streamMode.value == StreamMode.MJPEG && remaining == 0) {
                viewModelScope.launch(Dispatchers.Main) {
                    _isPreviewActive.value = false
                    unbindCamera()
                }
            }
        }

        cameraExecutor = Executors.newSingleThreadExecutor()
        startServer()
        schedulePreviewHide()
    }

    private var mjpegCamera: Camera? = null
    private var rtspCamera: RtspServerCamera2? = null

    private fun startServer() {
        if (!_isServerRunning.value) {
            VideoStreamServer.start(8080)
            _isServerRunning.value = true
            startActiveStream()
        }
    }

    private fun stopServer() {
        if (_isServerRunning.value) {
            VideoStreamServer.stop()
            _isServerRunning.value = false
            stopActiveStream()
        }
    }

    fun toggleServer() {
        if (_isServerRunning.value) {
            stopServer()
        } else {
            startServer()
        }
    }

    fun setStreamMode(mode: StreamMode) {
        if (_streamMode.value == mode) return
        viewModelScope.launch(Dispatchers.Main) {
            streamLifecycleMutex.withLock {
                stopActiveStream()
                delay(200)
                _streamMode.value = mode
                if (_isServerRunning.value) {
                    startActiveStream()
                }
            }
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    private fun startActiveStream() {
        when (_streamMode.value) {
            StreamMode.MJPEG -> {
                if (jpegViewerCount.get() > 0 || viewerCount.get() > 0) {
                    _isPreviewActive.value = true
                    bindCamera()
                }
            }
            StreamMode.H264_RTSP -> {
                startRtspCamera()
            }
        }
    }

    private fun stopActiveStream() {
        when (_streamMode.value) {
            StreamMode.MJPEG -> {
                unbindCamera()
                _isPreviewActive.value = false
            }
            StreamMode.H264_RTSP -> {
                stopRtspCamera()
            }
        }
    }

    fun tapToFocus(view: View, event: MotionEvent) {
        if (_streamMode.value == StreamMode.MJPEG) {
            val pView = previewView ?: return
            val factory = pView.meteringPointFactory
            val point = factory.createPoint(event.x, event.y)
            val action = FocusMeteringAction.Builder(point, FocusMeteringAction.FLAG_AF)
                .setAutoCancelDuration(3, java.util.concurrent.TimeUnit.SECONDS)
                .build()
            mjpegCamera?.cameraControl?.startFocusAndMetering(action)
        } else {
            rtspCamera?.let { camera ->
                try {
                    camera.tapToFocus(view, event)
                } catch (e: Exception) {
                    Log.e("AWA", "tapToFocus RTSP failed", e)
                }
            }
        }
    }

    fun tapToFocus(x: Float, y: Float, width: Float, height: Float) {
        val pView = previewView ?: return
        val factory = pView.meteringPointFactory
        val point = factory.createPoint(x, y)
        val action = FocusMeteringAction.Builder(point, FocusMeteringAction.FLAG_AF)
            .setAutoCancelDuration(3, java.util.concurrent.TimeUnit.SECONDS)
            .build()
        mjpegCamera?.cameraControl?.startFocusAndMetering(action)
    }

    fun toggleFocusMode() {
        val current = _settings.value.focusMode
        if (current == FocusMode.AUTO) {
            _settings.value = _settings.value.copy(focusMode = FocusMode.MANUAL)
        } else {
            cancelFocusAndMetering()
            _settings.value = _settings.value.copy(focusMode = FocusMode.AUTO)
        }
    }

    fun toggleFlash() {
        val newFlash = !_settings.value.isFlashEnabled
        applyFlash(newFlash)
    }

    fun notifyScreenTapped() {
        _showLocalPreview.value = true
        activePreviewSurface?.let { surface ->
            (rtspCamera?.glInterface as? GlStreamInterface)?.attachPreview(surface)
        }
        schedulePreviewHide()
    }

    private fun schedulePreviewHide() {
        hidePreviewRunnable?.let { mainHandler.removeCallbacks(it) }
        val runnable = Runnable { hideLocalPreview() }
        hidePreviewRunnable = runnable
        mainHandler.postDelayed(runnable, previewPeekDurationMs)
    }

    private fun hideLocalPreview() {
        _showLocalPreview.value = false
        (rtspCamera?.glInterface as? GlStreamInterface)?.deAttachPreview()
    }

    private fun buildConnectChecker() = object : ConnectChecker {
        override fun onConnectionStarted(url: String) {
            Log.d("AWA", "RTSP connection started: $url")
        }

        override fun onConnectionSuccess() {
            Log.d("AWA", "RTSP client connected successfully")
            _isPreviewActive.value = true
            viewerCount.incrementAndGet()
            VideoStreamServer.onUserConnected?.invoke()
        }

        override fun onConnectionFailed(reason: String) {
            Log.e("AWA", "RTSP connection failed: $reason")
            _isPreviewActive.value = false
        }

        override fun onNewBitrate(bitrate: Long) {
            Log.d("AWA", "RTSP new bitrate: $bitrate")
        }

        override fun onDisconnect() {
            Log.d("AWA", "RTSP client disconnected")
            _isPreviewActive.value = false
            viewerCount.updateAndGet { (it - 1).coerceAtLeast(0) }
            VideoStreamServer.onUserDisconnected?.invoke()
        }

        override fun onAuthError() {
            Log.e("AWA", "RTSP auth error")
        }

        override fun onAuthSuccess() {
            Log.d("AWA", "RTSP auth success")
        }
    }

    fun attachPreviewSurface(
        context: Context,
        owner: LifecycleOwner,
        view: PreviewView
    ) {
        lifecycleOwner = owner
        previewView = view
        if (_streamMode.value == StreamMode.MJPEG && _isServerRunning.value) {
            bindCamera()
        }
    }

    fun attachPreviewSurface(surface: Surface) {
        activePreviewSurface = surface
        if (_showLocalPreview.value) {
            (rtspCamera?.glInterface as? GlStreamInterface)?.attachPreview(surface)
        }
    }

    fun detachPreviewSurface() {
        activePreviewSurface = null
        (rtspCamera?.glInterface as? GlStreamInterface)?.deAttachPreview()
    }

    fun setPreviewResolution(width: Int, height: Int) {
        (rtspCamera?.glInterface as? GlStreamInterface)?.setPreviewResolution(width, height)
    }

    private fun startRtspCamera() {
        val context = appContext ?: return
        val s = _settings.value
        val camera = RtspServerCamera2(context, buildConnectChecker(), rtspPort)
        rtspCamera = camera

        var actualRes = s.resolution
        var videoOk = false
        val orientation = CameraHelper.getCameraOrientation(context)

        try {
            videoOk = camera.prepareVideo(
                actualRes.size.width,
                actualRes.size.height,
                30,
                rtspBitrate,
                1,
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
                    val fallbackOk = try {
                        camera.prepareVideo(fallback.size.width, fallback.size.height, 30, rtspBitrate, 1, orientation)
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
            val flashAvailable = camera.cameraCharacteristics
                ?.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) ?: false
            _settings.value = _settings.value.copy(hasFlashUnit = flashAvailable)

            camera.startStream()

            val gl = camera.glInterface as? GlStreamInterface
            if (_settings.value.rotationMode == "auto") {
                gl?.autoHandleOrientation = true
            } else {
                gl?.autoHandleOrientation = false
                val rot = _settings.value.rotationMode.toIntOrNull() ?: 0
                val isPortrait = (rot == 0 || rot == 180)
                gl?.setCameraOrientation(rot)
                gl?.setIsPortrait(isPortrait)
            }

            if (s.lensFacing == CameraSelector.LENS_FACING_FRONT && !camera.isFrontCamera) {
                try {
                    camera.switchCamera()
                } catch (e: Exception) {
                    Log.e("AWA", "switchCamera on start failed", e)
                }
            }
            Log.d("AWA", "RTSP server started on port $rtspPort")

            if (_showLocalPreview.value) {
                activePreviewSurface?.let { gl?.attachPreview(it) }
            }
        } else {
            Log.e("AWA", "RTSP: prepareVideo failed, not starting stream")
            rtspCamera = null
        }
    }

    private fun stopRtspCamera() {
        try {
            (rtspCamera?.glInterface as? GlStreamInterface)?.deAttachPreview()
            rtspCamera?.stopStream()
        } catch (e: Exception) {
            Log.e("AWA", "stopRtspCamera error", e)
        } finally {
            rtspCamera = null
        }
    }

    private fun restartRtspCamera(reason: String) {
        viewModelScope.launch(Dispatchers.Main) {
            streamLifecycleMutex.withLock {
                if (rtspCamera == null && !_isServerRunning.value) return@withLock
                Log.d("AWA", "Restarting RTSP camera: $reason")
                stopRtspCamera()
                delay(200)
                startRtspCamera()
            }
        }
    }

    private fun bindCamera() {
        val context = appContext ?: return
        val owner = lifecycleOwner ?: return
        val pView = previewView ?: return

        val s = _settings.value
        val cameraProviderFuture = ProcessCameraProvider.getInstance(context)

        cameraProviderFuture.addListener({
            try {
                val provider = cameraProviderFuture.get()
                cameraProvider = provider

                val cameraSelector = CameraSelector.Builder()
                    .requireLensFacing(s.lensFacing)
                    .build()

                val resolutionSelector = ResolutionSelector.Builder()
                    .setResolutionStrategy(
                        ResolutionStrategy(
                            s.resolution.size,
                            ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER
                        )
                    )
                    .setAspectRatioStrategy(
                        AspectRatioStrategy(
                            s.resolution.aspectRatio,
                            AspectRatioStrategy.FALLBACK_RULE_AUTO
                        )
                    )
                    .build()

                val previewUseCase = Preview.Builder()
                    .setResolutionSelector(resolutionSelector)
                    .build()
                    .also {
                        it.setSurfaceProvider(pView.surfaceProvider)
                    }

                val imageAnalysis = ImageAnalysis.Builder()
                    .setResolutionSelector(resolutionSelector)
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_RGBA_8888)
                    .build()

                imageAnalysisUseCase = imageAnalysis

                var lastProcessedTime = 0L
                val frameIntervalMs = 33L

                imageAnalysis.setAnalyzer(cameraExecutor!!) { imageProxy ->
                    val currentTime = System.currentTimeMillis()
                    if (currentTime - lastProcessedTime < frameIntervalMs) {
                        imageProxy.close()
                        return@setAnalyzer
                    }
                    lastProcessedTime = currentTime

                    try {
                        val bitmap = imageProxy.toBitmap()
                        val degrees = if (s.rotationMode == "auto") {
                            imageProxy.imageInfo.rotationDegrees.toFloat()
                        } else {
                            (s.rotationMode.toFloatOrNull() ?: 0f)
                        }

                        val rotatedBitmap = if (degrees != 0f) {
                            val matrix = Matrix().apply { postRotate(degrees) }
                            Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
                        } else {
                            bitmap
                        }

                        val out = ByteArrayOutputStream()
                        rotatedBitmap.compress(Bitmap.CompressFormat.JPEG, _settings.value.jpegQuality, out)
                        VideoStreamServer.pushMjpegFrame(out.toByteArray())
                        out.close()

                        if (rotatedBitmap != bitmap) {
                            rotatedBitmap.recycle()
                        }
                        bitmap.recycle()
                    } catch (e: Exception) {
                        Log.e("AWA", "Frame processing failed", e)
                    } finally {
                        imageProxy.close()
                    }
                }

                provider.unbindAll()
                val camera = provider.bindToLifecycle(owner, cameraSelector, previewUseCase, imageAnalysis)
                mjpegCamera = camera
                val exposureState = camera.cameraInfo.exposureState
                _settings.value = _settings.value.copy(
                    exposureRange = exposureState.exposureCompensationRange.lower..exposureState.exposureCompensationRange.upper,
                    exposureIndex = exposureState.exposureCompensationIndex,
                    hasFlashUnit = camera.cameraInfo.hasFlashUnit()
                )
            } catch (e: Exception) {
                Log.e("AWA", "CameraX binding failed", e)
            }
        }, ContextCompat.getMainExecutor(context))
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
                val facing = characteristics.get(CameraCharacteristics.LENS_FACING)
                if (facing == targetFacing) {
                    selectedCameraId = id
                    break
                }
            }

            if (selectedCameraId != null) {
                val characteristics = cameraManager.getCameraCharacteristics(selectedCameraId)
                val map = characteristics.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
                val outputSizes = map?.getOutputSizes(android.graphics.ImageFormat.JPEG) ?: emptyArray()

                val supported = StreamResolution.entries.filter { res ->
                    outputSizes.any { size ->
                        size.width == res.size.width && size.height == res.size.height
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

    private fun unbindCamera() {
        try {
            cameraProvider?.unbindAll()
            mjpegCamera = null
            imageAnalysisUseCase = null
        } catch (e: Exception) {
            Log.e("AWA", "CameraX unbind failed", e)
        }
    }

    fun setCameraFacing(front: Boolean) {
        val newFacing = if (front) CameraSelector.LENS_FACING_FRONT else CameraSelector.LENS_FACING_BACK
        if (_settings.value.lensFacing == newFacing) return
        _settings.value = _settings.value.copy(lensFacing = newFacing)
        appContext?.let { updateSupportedResolutions(it, newFacing) }
        viewModelScope.launch(Dispatchers.Main) {
            when (_streamMode.value) {
                StreamMode.MJPEG -> {
                    streamLifecycleMutex.withLock {
                        bindCamera()
                    }
                }
                StreamMode.H264_RTSP -> {
                    rtspCamera?.let { camera ->
                        try {
                            if (camera.isFrontCamera != front) {
                                camera.switchCamera()
                            }
                        } catch (e: Exception) {
                            Log.e("AWA", "camera.switchCamera failed, restarting RTSP", e)
                            restartRtspCamera("Camera facing changed")
                        }
                    }
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
        _settings.value = _settings.value.copy(
            focusDistance = distance,
            focusMode = FocusMode.MANUAL
        )
        if (_streamMode.value == StreamMode.H264_RTSP) {
            rtspCamera?.setFocusDistance(distance)
        }
    }

    fun cancelFocusAndMetering() {
        _settings.value = _settings.value.copy(focusMode = FocusMode.AUTO)
        mjpegCamera?.cameraControl?.cancelFocusAndMetering()
    }

    fun setRotation(mode: String) {
        _settings.value = _settings.value.copy(rotationMode = mode)
        when (_streamMode.value) {
            StreamMode.MJPEG -> bindCamera()
            StreamMode.H264_RTSP -> {
                (rtspCamera?.glInterface as? GlStreamInterface)?.let { gl ->
                    if (mode == "auto") {
                        gl.autoHandleOrientation = true
                    } else {
                        gl.autoHandleOrientation = false
                        val rot = mode.toIntOrNull() ?: 0
                        val isPortrait = (rot == 0 || rot == 180)
                        gl.setCameraOrientation(rot)
                        gl.setIsPortrait(isPortrait)
                    }
                }
            }
        }
        viewModelScope.launch {
            VideoStreamServer.broadcastSettingsUpdate()
        }
    }

    fun applyFlash(enabled: Boolean) {
        _settings.value = _settings.value.copy(isFlashEnabled = enabled)
        if (_streamMode.value == StreamMode.H264_RTSP) {
            rtspCamera?.let { camera ->
                try {
                    if (enabled) camera.enableLantern() else camera.disableLantern()
                } catch (e: Exception) {
                    Log.e("AWA", "Failed to set RTSP flash", e)
                }
            }
        } else {
            mjpegCamera?.cameraControl?.enableTorch(enabled)
        }
    }

    fun setResolution(res: StreamResolution) {
        if (_settings.value.resolution == res) return
        _settings.value = _settings.value.copy(resolution = res)
        when (_streamMode.value) {
            StreamMode.MJPEG -> {
                viewModelScope.launch(Dispatchers.Main) {
                    streamLifecycleMutex.withLock {
                        bindCamera()
                    }
                    VideoStreamServer.broadcastSettingsUpdate()
                }
            }
            StreamMode.H264_RTSP -> restartRtspCamera("Resolution changed to ${res.label}")
        }
    }

    fun setQuality(quality: Int) {
        _settings.value = _settings.value.copy(jpegQuality = quality.coerceIn(10, 100))
    }

    fun setZoom(ratio: Float) {
        val clamped = ratio.coerceIn(1.0f, 5.0f)
        viewModelScope.launch(Dispatchers.Main) {
            if (_streamMode.value == StreamMode.H264_RTSP) {
                rtspCamera?.setZoom(clamped)
            } else {
                mjpegCamera?.cameraControl?.setZoomRatio(clamped)
            }
            _settings.value = _settings.value.copy(zoom = clamped)
        }
    }

    fun setExposure(index: Int) {
        if (_streamMode.value == StreamMode.MJPEG) {
            mjpegCamera?.cameraControl?.setExposureCompensationIndex(index)
        } else {
            rtspCamera?.setExposure(index)
        }
        val clamped = index.coerceIn(_settings.value.exposureRange.first, _settings.value.exposureRange.last)
        _settings.value = _settings.value.copy(exposureIndex = clamped)
    }

    override fun onCleared() {
        super.onCleared()
        orientationEventListener?.disable()
        stopActiveStream()
        cameraExecutor?.shutdown()
        VideoStreamServer.stop()
    }
}
