package com.sjbtechnologies.awa.server

import android.util.Log
import io.ktor.http.*
import io.ktor.serialization.kotlinx.json.json
import io.ktor.server.application.*
import io.ktor.server.engine.*
import io.ktor.server.cio.*
import io.ktor.server.http.content.staticResources
import io.ktor.server.plugins.contentnegotiation.ContentNegotiation
import io.ktor.server.plugins.cors.routing.CORS
import io.ktor.server.websocket.*
import io.ktor.websocket.*
import io.ktor.server.request.receive
import io.ktor.server.response.*
import io.ktor.server.routing.*
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlin.time.Duration.Companion.seconds
import java.util.concurrent.CopyOnWriteArrayList

object VideoStreamServer {

    @Serializable
    data class FeaturesResponse(
        val resolutions: List<String>,
        val manual_focus: Boolean,
        val exposure_lower: Int,
        val exposure_upper: Int,
        val has_zoom: Boolean,
        val zoom_max: Float,
        val zoom_min: Float,
        val stream_protocol: String = "rtsp",
        val server_port: Int? = null,
        val rtsp_port: Int? = null,
        val rotation_options: List<String> = listOf("auto", "0", "90", "180", "270"),
        val current_rotation: String = "auto",
        val available_codecs: List<String> = listOf("h264", "h265"),
        val available_fps: List<Int> = listOf(15, 30)
    )

    @Serializable
    data class SettingsResponse(
        val camera: String,
        val resolution_str: String,
        val zoom: Float,
        val focus_mode: Int,
        val focus_distance: Float,
        val exposure_index: Int,
        val autofocus: Boolean? = null,
        val stream_quality: Int = 80,
        val flash: Boolean = false,
        val has_flash_unit: Boolean? = null,
        val stream_protocol: String = "rtsp",
        val rotation: String = "auto",
        val video_codec: String = "h264",
        val supported_resolutions: List<String> = emptyList(),
        val fps: Int = 30,
        val screen_off_streaming: Boolean = true
    )

    @Serializable
    data class SettingsUpdateRequest(
        val focus_mode: Int? = null,
        val focus_distance: Float? = null,
        val autofocus: Boolean? = null,
        val exposure_index: Int? = null,
        val zoom: Float? = null,
        val flash: Boolean? = null,
        val resolution_str: String? = null,
        val switchCamera: Boolean? = null,
        val camera: String? = null,
        val stream_quality: Int? = null,
        val stream_protocol: String? = null,
        val rotation: String? = null,
        val video_codec: String? = null,
        val fps: Int? = null,
        val screen_off_streaming: Boolean? = null
    )

    var featuresProvider: (() -> FeaturesResponse)? = null
    var settingsProvider: (() -> SettingsResponse)? = null

    // Suspends until the change has actually been applied, so the response and the
    // broadcast that follow reflect the new state. Returns null on success, or an
    // error message String if rejected.
    var onSettingsUpdated: (suspend (SettingsUpdateRequest) -> String?)? = null

    private var server: EmbeddedServer<*, *>? = null

    var onUserConnected: (() -> Unit)? = null
    var onUserDisconnected: (() -> Unit)? = null
    var onServerStateChanged: ((Boolean) -> Unit)? = null

    private val activeWsSessions = CopyOnWriteArrayList<DefaultWebSocketSession>()

    val isRunning: Boolean
        get() = server != null

    private val jsonSerializer = Json {
        prettyPrint = false
        ignoreUnknownKeys = true
        encodeDefaults = true
    }

    suspend fun broadcastSettingsUpdate() {
        val currentSettings = settingsProvider?.invoke() ?: return
        val jsonPayload = jsonSerializer.encodeToString(SettingsResponse.serializer(), currentSettings)
        for (session in activeWsSessions) {
            try {
                session.send(Frame.Text(jsonPayload))
            } catch (_: Exception) {
                activeWsSessions.remove(session)
            }
        }
    }

    fun toggleServer(port: Int = 8080) {
        if (isRunning) {
            stop()
        } else {
            start(port)
        }
        onServerStateChanged?.invoke(isRunning)
    }

    fun start(port: Int = 8080) {
        if (server != null) return

        server = embeddedServer(CIO, port = port) {
            install(ContentNegotiation) {
                json(
                    Json {
                        prettyPrint = true
                        ignoreUnknownKeys = true
                        encodeDefaults = true
                    }
                )
            }
            install(CORS) {
                anyHost()
                allowMethod(HttpMethod.Get)
                allowMethod(HttpMethod.Post)
                allowHeader(HttpHeaders.ContentType)
            }
            install(WebSockets) {
                pingPeriod = 15.seconds
                timeout = 15.seconds
                maxFrameSize = Long.MAX_VALUE
                masking = false
            }
            routing {
                webSocket("/ws") {
                    activeWsSessions.add(this)
                    onUserConnected?.invoke()
                    try {
                        // Immediately send full current state to newly connected client
                        settingsProvider?.invoke()?.let { currentSettings ->
                            val text = jsonSerializer.encodeToString(SettingsResponse.serializer(), currentSettings)
                            send(Frame.Text(text))
                        }

                        // Listen for real-time incoming commands from client
                        for (frame in incoming) {
                            if (frame is Frame.Text) {
                                val text = frame.readText()
                                Log.d("AWA-WS", "Received WS command: $text")
                                try {
                                    val update = jsonSerializer.decodeFromString(SettingsUpdateRequest.serializer(), text)
                                    onSettingsUpdated?.invoke(update)
                                    broadcastSettingsUpdate()
                                } catch (e: Exception) {
                                    Log.e("AWA-WS", "Error parsing command: ${e.message}")
                                }
                            }
                        }
                    } catch (e: Exception) {
                        Log.d("AWA-WS", "WS session error / closed: ${e.message}")
                    } finally {
                        activeWsSessions.remove(this)
                        onUserDisconnected?.invoke()
                    }
                }

                get("/features") {
                    val response = featuresProvider?.invoke() ?: FeaturesResponse(
                        resolutions = listOf("640x480", "1280x720", "1920x1080"),
                        manual_focus = false,
                        exposure_upper = 0,
                        exposure_lower = 0,
                        has_zoom = false,
                        zoom_max = 1.0f,
                        zoom_min = 1.0f,
                        stream_protocol = "rtsp",
                        server_port = 8080,
                        rtsp_port = 8554
                    )
                    call.respond(response)
                }

                get("/settings") {
                    val response = settingsProvider?.invoke() ?: SettingsResponse(
                        camera = "back",
                        resolution_str = "1280x720",
                        zoom = 1.0f,
                        flash = false,
                        exposure_index = 1,
                        autofocus = true,
                        focus_mode = 0,
                        focus_distance = 0f,
                        has_flash_unit = false,
                        stream_quality = 80,
                        stream_protocol = "rtsp",
                        rotation = "auto",
                        video_codec = "h264",
                        fps = 30
                    )
                    call.respond(response)
                }

                // POST Endpoint
                post("/settings") {
                    try {
                        val request = call.receive<SettingsUpdateRequest>()
                        val error = onSettingsUpdated?.invoke(request)
                        if (error != null) {
                            call.respond(HttpStatusCode.BadRequest, mapOf("error" to error))
                        } else {
                            broadcastSettingsUpdate()
                            call.respond(HttpStatusCode.OK, mapOf("status" to "success"))
                        }
                    } catch (e: Exception) {
                        call.respond(HttpStatusCode.BadRequest, mapOf("error" to (e.message ?: "Invalid payload")))
                    }
                }

                // GET Endpoint
                get("/control") {
                    val params = call.request.queryParameters
                    Log.d("AWA", "Control query: $params")
                    val update = SettingsUpdateRequest(
                        camera = params["camera"],
                        resolution_str = params["resolution_str"],
                        zoom = params["zoom"]?.toFloatOrNull(),
                        flash = params["flash"]?.toBooleanStrictOrNull(),
                        exposure_index = params["exposure_index"]?.toIntOrNull(),
                        autofocus = params["autofocus"]?.toBooleanStrictOrNull(),
                        focus_mode = params["focus_mode"]?.toIntOrNull(),
                        focus_distance = params["focus_distance"]?.toFloatOrNull(),
                        switchCamera = if (params.contains("switch_camera") || params["switchCamera"] == "true") true else null,
                        stream_quality = params["stream_quality"]?.toIntOrNull() ?: 80,
                        stream_protocol = params["stream_protocol"],
                        rotation = params["rotation"],
                        video_codec = params["video_codec"],
                        fps = params["fps"]?.toIntOrNull(),
                        screen_off_streaming = params["screen_off_streaming"]?.toBooleanStrictOrNull()
                    )

                    val error = onSettingsUpdated?.invoke(update)
                    if (error != null) {
                        call.respond(HttpStatusCode.BadRequest, mapOf("error" to error))
                    } else {
                        broadcastSettingsUpdate()
                        val response = settingsProvider?.invoke() ?: SettingsResponse(
                            focus_mode = 0, focus_distance = 0f, exposure_index = 1,
                            zoom = 1.0f, stream_quality = 80, resolution_str = "1280x720",
                            camera = "back", stream_protocol = "rtsp", rotation = "auto",
                            video_codec = "h264", fps = 30
                        )
                        call.respond(HttpStatusCode.OK, response)
                    }
                }
                staticResources("/static", "static")
                get("/help"){
                    call.respondRedirect("/static/help.html")
                }
            }
        }.start(wait = false)
    }

    fun stop() {
        activeWsSessions.clear()
        server?.stop(1000, 2000)
        server = null
    }
}
