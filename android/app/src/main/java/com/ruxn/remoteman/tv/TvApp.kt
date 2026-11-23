package com.ruxn.remoteman.tv

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.unit.dp
import androidx.tv.material3.MaterialTheme
import androidx.tv.material3.Surface
import androidx.tv.material3.SurfaceDefaults
import androidx.tv.material3.darkColorScheme
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Screen

/**
 * TVs: Compose for TV, worked with the remote's D-pad. Every screen puts focus somewhere when
 * it opens (Compose focuses nothing by itself), and Back goes back.
 */
@Composable
fun TvApp(model: AppModel) {
    MaterialTheme(colorScheme = darkColorScheme()) {
        // The few phone components used here (text fields) take their colours from this.
        androidx.compose.material3.MaterialTheme(colorScheme = androidx.compose.material3.darkColorScheme()) {
            BackHandler(enabled = model.stack.size > 1) { model.back() }
            val s = model.screen
            if (s == Screen.Session) {
                TvSession(model)
            } else {
                // TV overscan: keep 48 by 27 dp clear of the screen's edges.
                // The TV Surface sets the content colour that Text takes.
                Surface(Modifier.fillMaxSize(), colors = SurfaceDefaults.colors(containerColor = MaterialTheme.colorScheme.background)) {
                  Box(Modifier.fillMaxSize().padding(horizontal = 48.dp, vertical = 27.dp)) {
                    when (s) {
                        Screen.Computers -> TvComputers(model)
                        Screen.Add, Screen.Scan -> TvAdd(model)
                        is Screen.Computer -> TvComputer(model, s.id)
                        Screen.Settings -> TvSettings(model)
                        Screen.Session -> {}
                    }
                  }
                }
            }
        }
    }
}

/** A FocusRequester that takes focus once its element is on screen. */
@Composable
fun rememberInitialFocus(key: Any? = Unit): FocusRequester {
    val f = remember(key) { FocusRequester() }
    LaunchedEffect(key) { runCatching { f.requestFocus() } }
    return f
}
