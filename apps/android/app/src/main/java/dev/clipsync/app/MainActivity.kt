package dev.clipsync.app

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private val Ink = Color(0xFF121416)
private val Panel = Color(0xFF1E2124)
private val Mint = Color(0xFF4FE3C2)

class MainActivity : ComponentActivity() {
    private val notifyPerm = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33) {
            notifyPerm.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        val app = application as ClipSyncApp
        setContent { ClipSyncScreen(app) }
    }
}

@Composable
private fun ClipSyncScreen(app: ClipSyncApp) {
    val scope = rememberCoroutineScope()
    var status by remember {
        mutableStateOf(
            runCatching { app.client.status() }.getOrNull()
                ?.let { it.roomId?.let { id -> "Paired · ${id.take(8)}…" } ?: "Not paired" }
                ?: "Starting…"
        )
    }
    var code by remember { mutableStateOf<String?>(null) }
    var join by remember { mutableStateOf("") }
    var room by remember {
        mutableStateOf(runCatching { app.client.status().roomId }.getOrNull())
    }
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }

    fun startService() {
        val ctx = app.applicationContext
        ContextCompat.startForegroundService(ctx, Intent(ctx, SyncService::class.java))
    }

    Column(
        Modifier
            .fillMaxSize()
            .background(Ink)
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp)
    ) {
        Text("CLIPSYNC", color = Mint, fontFamily = FontFamily.Monospace, letterSpacing = 2.sp, fontSize = 13.sp)
        Text("Native clipboard", color = Color.White, fontSize = 28.sp, fontWeight = FontWeight.SemiBold)
        Text(status, color = Color.White.copy(alpha = 0.65f), fontFamily = FontFamily.Monospace)

        if (code != null && room == null) {
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(Panel, RoundedCornerShape(20.dp))
                    .padding(24.dp)
            ) {
                Text("PAIRING CODE", color = Mint, fontFamily = FontFamily.Monospace, fontSize = 11.sp)
                Spacer(Modifier.height(8.dp))
                Text(code!!, color = Color.White, fontSize = 44.sp, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Bold)
            }
        }

        if (room == null) {
            Button(
                onClick = {
                    busy = true
                    error = null
                    scope.launch {
                        try {
                            val offer = withContext(Dispatchers.IO) { app.client.createRoom() }
                            code = offer.pairingCode
                            status = "Waiting for join…"
                        } catch (e: Exception) {
                            error = e.message
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy,
                colors = ButtonDefaults.buttonColors(containerColor = Mint, contentColor = Ink),
                modifier = Modifier.fillMaxWidth()
            ) { Text("Create room") }

            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                OutlinedTextField(
                    value = join,
                    onValueChange = { join = it.filter { ch -> ch.isDigit() }.take(6) },
                    modifier = Modifier.weight(1f),
                    label = { Text("Join code") },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedTextColor = Color.White,
                        unfocusedTextColor = Color.White
                    )
                )
                Button(
                    onClick = {
                        busy = true
                        error = null
                        scope.launch {
                            try {
                                val id = withContext(Dispatchers.IO) { app.client.joinRoom(join) }
                                room = id
                                status = "Paired"
                                startService()
                            } catch (e: Exception) {
                                error = e.message
                            } finally {
                                busy = false
                            }
                        }
                    },
                    enabled = !busy && join.length == 6,
                    colors = ButtonDefaults.buttonColors(containerColor = Mint, contentColor = Ink)
                ) { Text("Join") }
            }
        } else {
            Text(
                "Copy on this phone or a paired computer. Keep ClipSync running so the clipboard stays in sync.",
                color = Color.White.copy(alpha = 0.7f)
            )
            Button(
                onClick = { startService() },
                colors = ButtonDefaults.buttonColors(containerColor = Mint, contentColor = Ink)
            ) { Text("Start sync service") }
        }

        error?.let { Text(it, color = Color(0xFFFF7366), fontFamily = FontFamily.Monospace, fontSize = 13.sp) }
    }
}
