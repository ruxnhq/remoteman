package com.ruxn.remoteman.phone

import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Screen

/** Phones and tablets: Material 3, in the device's own colours where it has them (Android 12+). */
@Composable
fun PhoneApp(model: AppModel) {
    val dark = isSystemInDarkTheme()
    val ctx = LocalContext.current
    val colors = when {
        Build.VERSION.SDK_INT >= 31 && dark -> dynamicDarkColorScheme(ctx)
        Build.VERSION.SDK_INT >= 31 -> dynamicLightColorScheme(ctx)
        dark -> darkColorScheme()
        else -> lightColorScheme()
    }
    MaterialTheme(colorScheme = colors) {
        BackHandler(enabled = model.stack.size > 1) { model.back() }
        when (val s = model.screen) {
            Screen.Computers -> ComputersScreen(model)
            Screen.Add -> AddScreen(model)
            is Screen.Computer -> ComputerScreen(model, s.id)
            Screen.Settings -> SettingsScreen(model)
            Screen.Scan -> ScanScreen(model)
            Screen.Session -> MaterialTheme(colorScheme = if (Build.VERSION.SDK_INT >= 31) dynamicDarkColorScheme(ctx) else darkColorScheme()) {
                PhoneSession(model)
            }
        }
    }
}
