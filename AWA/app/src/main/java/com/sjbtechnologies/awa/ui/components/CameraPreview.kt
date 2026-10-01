package com.sjbtechnologies.awa.ui.components

import android.view.MotionEvent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import com.pedro.library.view.OpenGlView
import com.sjbtechnologies.awa.viewModel.CameraViewModel

@Composable
fun Preview(
    viewModel: CameraViewModel,
    modifier: Modifier = Modifier
) {
    var boundView by remember { mutableStateOf<OpenGlView?>(null) }
    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            OpenGlView(ctx).apply {
                setOnTouchListener { view, event ->
                    if (event.action == MotionEvent.ACTION_UP) {
                        viewModel.tapToFocus(view, event)
                        viewModel.notifyScreenTapped()
                    }
                    true
                }
                viewModel.attachOpenGlView(this)
                boundView = this
            }
        },
        update = { openGlView ->
            viewModel.attachOpenGlView(openGlView)
            boundView = openGlView
        },
        onRelease = { view ->
            if (boundView === view) {
                viewModel.detachOpenGlView()
                boundView = null
            }
        }
    )
    DisposableEffect(Unit) {
        onDispose {
            viewModel.detachOpenGlView()
        }
    }
}
