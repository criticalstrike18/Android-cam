package com.sjbtechnologies.awa.ui.components

import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.camera.view.PreviewView
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.viewinterop.AndroidView
import com.sjbtechnologies.awa.viewModel.CameraViewModel

@Composable
fun Preview(
    viewModel: CameraViewModel,
    modifier: Modifier = Modifier
) {
    val streamMode by viewModel.streamMode
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current

    if (streamMode == CameraViewModel.StreamMode.MJPEG) {
        AndroidView(
            modifier = modifier,
            factory = { ctx ->
                PreviewView(ctx).apply {
                    viewModel.attachPreviewSurface(
                        context,
                        lifecycleOwner,
                        this
                    )
                }
            }
        )
    } else {
        val surfaceView = remember {
            SurfaceView(context)
        }

        AndroidView(
            modifier = modifier,
            factory = {
                surfaceView.apply {
                    setOnTouchListener { view, event ->
                        if (event.action == MotionEvent.ACTION_UP) {
                            viewModel.tapToFocus(view, event)
                            viewModel.notifyScreenTapped()
                        }
                        true
                    }
                    holder.addCallback(object : SurfaceHolder.Callback {
                        override fun surfaceCreated(holder: SurfaceHolder) {
                            Log.d("AWA", "Preview surface created")
                            viewModel.attachPreviewSurface(holder.surface)
                        }

                        override fun surfaceChanged(
                            holder: SurfaceHolder,
                            format: Int,
                            width: Int,
                            height: Int
                        ) {
                            viewModel.setPreviewResolution(width, height)
                        }

                        override fun surfaceDestroyed(holder: SurfaceHolder) {
                            Log.d("AWA", "Preview surface destroyed")
                            viewModel.detachPreviewSurface()
                        }
                    })
                }
            }
        )

        DisposableEffect(Unit) {
            onDispose {
                viewModel.detachPreviewSurface()
            }
        }
    }
}
