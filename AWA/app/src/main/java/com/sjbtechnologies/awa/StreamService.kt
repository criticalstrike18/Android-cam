package com.sjbtechnologies.awa

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import androidx.core.app.NotificationCompat

/**
 * Keeps the app process alive across background/screen-off with a return/stop notification.
 *
 * What it does NOT do: keep the camera session alive. The GL pipeline is display-bound — when
 * the window hides, the SurfaceView surface is destroyed and the encoder starves, and holding
 * that half-dead session wedges CamX past in-app recovery (verified empirically). So the
 * activity always stops the session cleanly first; this service exists so resume is an instant
 * restart instead of a cold start, and so the user has a one-tap return and a Stop action.
 *
 * Owns no camera state itself.
 */
class StreamService : Service() {

    companion object {
        const val ACTION_STOP = "com.sjbtechnologies.awa.STREAM_STOP"
        private const val NOTIF_ID = 41
        private const val CHANNEL_ID = "awa_stream"

        fun start(context: Context) {
            val intent = Intent(context, StreamService::class.java)
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            } catch (e: Exception) {
                Log.e("AWA", "StreamService start failed", e)
            }
        }

        fun stop(context: Context) {
            try {
                context.stopService(Intent(context, StreamService::class.java))
            } catch (e: Exception) {
                Log.e("AWA", "StreamService stop failed", e)
            }
        }

        fun stopIntent(context: Context): PendingIntent {
            val intent = Intent(context, StreamService::class.java).setAction(ACTION_STOP)
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) PendingIntent.FLAG_IMMUTABLE else 0)
            return PendingIntent.getService(context, 0, intent, flags)
        }
    }

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Camera streaming",
                NotificationManager.IMPORTANCE_LOW
            )
            channel.description = "Keeps the webcam stream alive with the screen off"
            manager.createNotificationChannel(channel)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            Log.d("AWA", "StreamService: Stop requested from notification")
            shutdownStream()
            stopSelf()
            return START_NOT_STICKY
        }

        try {
            val notification = buildNotification()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(NOTIF_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA)
            } else {
                startForeground(NOTIF_ID, notification)
            }
            acquireWakeLock()
            Log.d("AWA", "StreamService: foreground, camera session held")
        } catch (e: Exception) {
            Log.e("AWA", "StreamService: startForeground failed", e)
            stopSelf()
        }
        // If the system kills us, do NOT resurrect without the activity: a stale service
        // with no session would squat on the camera.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        releaseWakeLock()
        Log.d("AWA", "StreamService destroyed")
        super.onDestroy()
    }

    private fun buildNotification(): Notification {
        val openApp = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) PendingIntent.FLAG_IMMUTABLE else 0)
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("AWA streaming")
            .setContentText("Camera is live with the screen off. Tap Stop to end the stream.")
            .setSmallIcon(android.R.drawable.presence_video_online)
            .setContentIntent(openApp)
            .setOngoing(true)
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                "Stop",
                stopIntent(this)
            )
            .build()
    }

    private fun acquireWakeLock() {
        try {
            val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "AWA:stream").apply {
                acquire(60 * 60 * 1000L /* 1h max */)
            }
        } catch (e: Exception) {
            Log.e("AWA", "Wake lock acquire failed", e)
        }
    }

    private fun releaseWakeLock() {
        try {
            wakeLock?.let { if (it.isHeld) it.release() }
        } catch (e: Exception) {
            Log.e("AWA", "Wake lock release failed", e)
        } finally {
            wakeLock = null
        }
    }

    private fun shutdownStream() {
        // Best effort: tell the shared ViewModel state to stop. The activity-owned
        // ViewModel is the source of truth; this only covers stop-from-notification
        // while the activity is dead.
        try {
            StreamServiceStop.requestStop()
        } catch (e: Exception) {
            Log.e("AWA", "Stop request failed", e)
        }
    }
}

/**
 * Process-wide stop flag so the notification's Stop action works even when the
 * activity (and its ViewModel) is gone. The ViewModel checks and clears it.
 */
object StreamServiceStop {
    @Volatile var stopRequested: Boolean = false
        private set

    fun requestStop() {
        stopRequested = true
    }

    fun consumeStop(): Boolean {
        val v = stopRequested
        stopRequested = false
        return v
    }
}
