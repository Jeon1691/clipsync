package dev.clipsync.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build
import uniffi.clipsync_mobile.ClipSyncClient

class ClipSyncApp : Application() {
    lateinit var client: ClipSyncClient
        private set

    override fun onCreate() {
        super.onCreate()
        if (Build.VERSION.SDK_INT >= 26) {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "ClipSync", NotificationManager.IMPORTANCE_LOW)
            )
        }
        val dir = filesDir.resolve("clipsync").apply { mkdirs() }
        client = ClipSyncClient(
            dataDir = dir.absolutePath,
            relayUrl = "https://clipsync.develicit.dev",
            os = "android",
            deviceName = Build.MODEL ?: "android",
        )
    }

    companion object {
        const val CHANNEL_ID = "clipsync-sync"
    }
}
